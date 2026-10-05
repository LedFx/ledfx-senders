//! Owned Python binding around the private authenticated DTLS adapter.
#![allow(dead_code)]
mod config;
mod io;
mod session;

use crate::{buffer::NumericPolicy, encoders, test_gate};
use config::{Cancellation, HueConfig, HueError};
use io::Client;
use pyo3::{
    exceptions::{PyConnectionError, PyOSError, PyRuntimeError, PyTimeoutError, PyValueError},
    prelude::*,
    types::PyMemoryView,
};
use std::net::{IpAddr, SocketAddr};
use std::sync::{
    Arc, Mutex, MutexGuard, TryLockError,
    atomic::{AtomicU8, Ordering},
};
use std::time::{Duration, Instant};

const NEW: u8 = 0;
const CONNECTING: u8 = 1;
const CONNECTED: u8 = 2;
const FAILED: u8 = 3;
const CLOSED: u8 = 4;
const POLL: Duration = Duration::from_millis(20);

fn error(error: HueError) -> PyErr {
    match error {
        HueError::Configuration(_) => PyValueError::new_err("invalid Hue configuration"),
        HueError::Protocol(_) => PyConnectionError::new_err("Hue DTLS operation failed"),
        HueError::Io(_) => PyOSError::new_err("Hue socket operation failed"),
        HueError::Timeout => PyTimeoutError::new_err("Hue operation timed out"),
        HueError::Closed => PyConnectionError::new_err("Hue session is unavailable"),
    }
}

fn duration(seconds: f64) -> Result<Duration, HueError> {
    let value = Duration::try_from_secs_f64(seconds)
        .map_err(|_| HueError::Configuration("invalid Hue timeout"))?;
    if value.is_zero() || Instant::now().checked_add(value).is_none() {
        return Err(HueError::Configuration("invalid Hue timeout"));
    }
    Ok(value)
}

pub struct HueLayout {
    template: Vec<u8>,
    count: usize,
}

impl HueLayout {
    pub fn new(identifier: [u8; 36], ids: Vec<u8>, sequence: u8) -> Result<Self, HueError> {
        if !(1..=256).contains(&ids.len())
            || ids.iter().enumerate().any(|(i, id)| ids[..i].contains(id))
            || identifier.iter().enumerate().any(|(i, b)| {
                if [8, 13, 18, 23].contains(&i) {
                    *b != b'-'
                } else {
                    !b.is_ascii_hexdigit()
                }
            })
        {
            return Err(HueError::Configuration("invalid Hue layout"));
        }
        let count = ids.len();
        let mut template = Vec::with_capacity(52 + count * 7);
        template.extend_from_slice(b"HueStream");
        template.extend_from_slice(&[2, 0, sequence, 0, 0, 0, 0]);
        template.extend_from_slice(&identifier);
        for id in ids {
            template.extend_from_slice(&[id, 0, 0, 0, 0, 0, 0]);
        }
        Ok(Self { template, count })
    }

    pub fn pack(&self, rgb: &[u8]) -> Vec<u8> {
        let mut output = self.template.clone();
        encoders::fill_hue_rgb(&mut output, rgb);
        output
    }
}

#[pyclass]
pub struct HueEngine {
    layout: HueLayout,
    config: Mutex<Option<HueConfig>>,
    inner: Mutex<Option<Client>>,
    cancel: Arc<Cancellation>,
    state: AtomicU8,
    connect_timeout: Duration,
    send_timeout: Duration,
    close_timeout: Duration,
}

impl HueEngine {
    #[allow(clippy::too_many_arguments)]
    fn build(
        destination: &str,
        port: u16,
        identity: &[u8],
        key: &[u8],
        identifier: &[u8],
        ids: &[u8],
        sequence: u8,
        connect_timeout: f64,
        send_timeout: f64,
        close_timeout: f64,
    ) -> Result<Self, HueError> {
        let address: IpAddr = destination
            .parse()
            .map_err(|_| HueError::Configuration("numeric IP address required"))?;
        let connect_timeout = duration(connect_timeout)?;
        let send_timeout = duration(send_timeout)?;
        let close_timeout = duration(close_timeout)?;
        let config = HueConfig::new(
            SocketAddr::new(address, port),
            identity.to_vec(),
            key.to_vec(),
            connect_timeout,
            send_timeout,
            close_timeout,
        )?;
        let identifier = identifier
            .try_into()
            .map_err(|_| HueError::Configuration("invalid Hue layout"))?;
        let layout = HueLayout::new(identifier, ids.to_vec(), sequence)?;
        Ok(Self {
            layout,
            config: Mutex::new(Some(config)),
            inner: Mutex::new(None),
            cancel: Arc::new(Cancellation::new()),
            state: AtomicU8::new(NEW),
            connect_timeout,
            send_timeout,
            close_timeout,
        })
    }

    fn deadline(&self, timeout: Duration) -> Result<Instant, HueError> {
        Instant::now()
            .checked_add(timeout)
            .ok_or(HueError::Configuration("invalid deadline"))
    }

    fn lock(
        &self,
        deadline: Instant,
        closing: bool,
    ) -> Result<MutexGuard<'_, Option<Client>>, HueError> {
        loop {
            if !closing && self.cancel.cancelled() {
                return Err(HueError::Closed);
            }
            if Instant::now() >= deadline {
                return Err(HueError::Timeout);
            }
            match self.inner.try_lock() {
                Ok(guard) => return Ok(guard),
                Err(TryLockError::Poisoned(_)) => return Err(HueError::Closed),
                Err(TryLockError::WouldBlock) => {
                    let until = (Instant::now() + POLL).min(deadline);
                    if closing {
                        std::thread::sleep(until.saturating_duration_since(Instant::now()));
                    } else {
                        self.cancel.wait(until)?;
                    }
                }
            }
        }
    }

    fn session_lock(&self, deadline: Instant) -> Result<MutexGuard<'_, Option<Client>>, HueError> {
        self.lock(deadline, false).inspect_err(|error| {
            if matches!(error, HueError::Timeout) {
                let _ = self
                    .state
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                        if state == CLOSED { None } else { Some(FAILED) }
                    });
                self.cancel.cancel();
                self.config.lock().unwrap_or_else(|e| e.into_inner()).take();
                // A concurrent active owner sees cancellation and disposes its client.
            }
        })
    }

    fn publish_connected(&self) -> Result<(), HueError> {
        if self.cancel.cancelled() {
            return Err(HueError::Closed);
        }
        self.state
            .compare_exchange(CONNECTING, CONNECTED, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| HueError::Closed)
    }

    // Called while the active owner holds the session mutex, on every exit.
    fn finish(
        &self,
        client: &mut Option<Client>,
        result: Result<(), HueError>,
    ) -> Result<(), HueError> {
        if self.cancel.cancelled() || result.is_err() {
            client.take();
            self.config.lock().unwrap_or_else(|e| e.into_inner()).take();
            let _ = self
                .state
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                    if state == CLOSED { None } else { Some(FAILED) }
                });
            self.cancel.cancel();
            return result.and(Err(HueError::Closed));
        }
        result
    }

    fn send_owned(&self, packet: Vec<u8>, deadline: Instant) -> Result<(), HueError> {
        let mut client = self.session_lock(deadline)?;
        if self.state.load(Ordering::Acquire) == NEW && !self.cancel.cancelled() {
            return Err(HueError::Closed);
        }
        let result = if self.state.load(Ordering::Acquire) != CONNECTED {
            Err(HueError::Closed)
        } else {
            match client.as_mut() {
                Some(client) => client.send(&packet, deadline, &self.cancel),
                None => Err(HueError::Closed),
            }
        };
        self.finish(&mut client, result)
    }

    fn snapshot(&self, py: Python<'_>, frame: &Bound<'_, PyAny>, kind: u8) -> PyResult<Vec<u8>> {
        let view = PyMemoryView::from(frame)?;
        let mut channels = encoders::Channels::default();
        channels.snapshot(py, &view, kind, self.layout.count * 3)?;
        let packet = py.detach(|| {
            channels
                .convert(NumericPolicy::Strict)
                .map(|rgb| self.layout.pack(rgb))
        });
        // Release exporter while attached, before any engine/session mutex.
        drop(view);
        packet.map_err(encoders::error)
    }
}

#[pymethods]
impl HueEngine {
    #[new]
    #[allow(clippy::too_many_arguments)]
    fn new(
        destination: &str,
        port: u16,
        identity: &[u8],
        key: &[u8],
        identifier: &[u8],
        ids: &[u8],
        sequence: u8,
        connect_timeout: f64,
        send_timeout: f64,
        close_timeout: f64,
    ) -> PyResult<Self> {
        Self::build(
            destination,
            port,
            identity,
            key,
            identifier,
            ids,
            sequence,
            connect_timeout,
            send_timeout,
            close_timeout,
        )
        .map_err(error)
    }

    fn connect(&self, py: Python<'_>) -> PyResult<()> {
        if self.connected() {
            return Ok(());
        }
        let deadline = self.deadline(self.connect_timeout).map_err(error)?;
        py.detach(|| {
            let mut client = self.session_lock(deadline)?;
            match self.state.load(Ordering::Acquire) {
                CONNECTED => return self.finish(&mut client, Ok(())),
                NEW => {}
                _ => return self.finish(&mut client, Err(HueError::Closed)),
            }
            if self
                .state
                .compare_exchange(NEW, CONNECTING, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return self.finish(&mut client, Err(HueError::Closed));
            }
            let config = self
                .config
                .lock()
                .map_err(|_| HueError::Closed)?
                .take()
                .ok_or(HueError::Closed)?;
            let result = match Client::connect(config, Arc::clone(&self.cancel), deadline) {
                Ok(connected) => {
                    *client = Some(connected);
                    // Close may publish cancellation while the handshake completes.
                    self.publish_connected()
                }
                Err(error) => Err(error),
            };
            self.finish(&mut client, result)
        })
        .map_err(error)
    }

    fn send(&self, py: Python<'_>, frame: &Bound<'_, PyAny>, kind: u8) -> PyResult<()> {
        let deadline = self.deadline(self.send_timeout).map_err(error)?;
        let packet = self.snapshot(py, frame, kind)?;
        py.detach(|| self.send_owned(packet, deadline))
            .map_err(error)
    }

    fn service(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| {
            let mut client = match self.inner.try_lock() {
                Ok(client) => client,
                Err(TryLockError::WouldBlock) => return Ok(()),
                Err(TryLockError::Poisoned(_)) => return Err(HueError::Closed),
            };
            if self.state.load(Ordering::Acquire) == NEW {
                return Ok(());
            }
            let result = match client.as_mut() {
                Some(client) => client.service(Instant::now(), &self.cancel),
                None => Err(HueError::Closed),
            };
            self.finish(&mut client, result)
        })
        .map_err(error)
    }

    fn close(&self, py: Python<'_>) -> PyResult<()> {
        let deadline = self.deadline(self.close_timeout).map_err(error)?;
        self.state.store(CLOSED, Ordering::Release);
        py.detach(|| {
            self.cancel.cancel();
            let mut client = self.lock(deadline, true)?;
            self.config.lock().map_err(|_| HueError::Closed)?.take();
            let result = match client.as_mut() {
                Some(client) => client.close(deadline),
                None => Ok(()),
            };
            client.take();
            result
        })
        .map_err(error)
    }

    #[getter]
    fn connected(&self) -> bool {
        self.state.load(Ordering::Acquire) == CONNECTED && !self.cancel.cancelled()
    }
    #[getter]
    fn closed(&self) -> bool {
        self.state.load(Ordering::Acquire) == CLOSED
    }

    fn _test_hold_lock(&self, py: Python<'_>, gate: &test_gate::TestLockGate) -> PyResult<()> {
        py.detach(|| {
            let mut client = self.inner.lock().map_err(|_| "engine lock poisoned")?;
            let result = gate.shared.hold();
            if self.cancel.cancelled() {
                let _ = self.finish(&mut client, Err(HueError::Closed));
            }
            result
        })
        .map_err(PyRuntimeError::new_err)
    }

    fn _test_send_after_snapshot(
        &self,
        py: Python<'_>,
        frame: &Bound<'_, PyAny>,
        kind: u8,
        gate: &test_gate::TestLockGate,
    ) -> PyResult<()> {
        let deadline = self.deadline(self.send_timeout).map_err(error)?;
        let packet = self.snapshot(py, frame, kind)?;
        py.detach(|| gate.shared.hold())
            .map_err(PyRuntimeError::new_err)?;
        py.detach(|| self.send_owned(packet, deadline))
            .map_err(error)
    }
}
#[cfg(test)]
mod binding_tests {
    use super::*;

    #[test]
    fn layout_matches_literal_and_preserves_uppercase() {
        let identifier = *b"12345678-1234-1234-1234-123456789ABC";
        let layout = HueLayout::new(identifier, vec![7], 254).unwrap();
        let mut expected = b"HueStream\x02\0\xfe\0\0\0\0".to_vec();
        expected.extend_from_slice(&identifier);
        expected.extend_from_slice(&[7, 1, 1, 2, 2, 255, 255]);
        assert_eq!(layout.pack(&[1, 2, 255]), expected);
        assert!(HueLayout::new(identifier, vec![], 0).is_err());
        assert!(HueLayout::new(identifier, vec![7, 7], 0).is_err());
    }

    #[test]
    fn constructor_has_no_client_or_socket_and_rejects_overflowing_budget() {
        let engine = HueEngine::build(
            "::1",
            2100,
            b"id\0opaque",
            &[0; 16],
            b"12345678-1234-1234-1234-123456789abc",
            &[7],
            0,
            5.0,
            0.2,
            0.2,
        )
        .unwrap();
        assert!(engine.inner.lock().unwrap().is_none());
        assert_eq!(engine.state.load(Ordering::Acquire), NEW);
        assert!(duration(1e308).is_err());
        assert!(duration(0.0).is_err());
    }
    #[test]
    fn embedded_nul_identity_authenticates_with_rust_only_peer() {
        use bytes::BytesMut;
        use rtc_dtls::{
            cipher_suite::CipherSuiteId,
            config::ConfigBuilder,
            endpoint::{Endpoint, EndpointEvent},
        };
        use rtc_shared::TransportProtocol;
        let engine = HueEngine::build(
            "127.0.0.1",
            2100,
            b"id\0opaque",
            &[42; 16],
            b"12345678-1234-1234-1234-123456789abc",
            &[7],
            0,
            5.0,
            0.2,
            0.2,
        )
        .unwrap();
        let config = engine.config.lock().unwrap().take().unwrap();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let peer_observed = Arc::clone(&observed);
        let server_config = ConfigBuilder::default()
            .with_crypto_provider(Arc::new(rtc_crypto::providers::RingProvider::new()))
            .with_cipher_suites(vec![CipherSuiteId::Tls_Psk_With_Aes_128_Gcm_Sha256])
            .with_psk(Some(Arc::new(move |identity| {
                *peer_observed.lock().unwrap() = identity.to_vec();
                Ok(vec![42; 16])
            })))
            .build(false, None)
            .unwrap();
        let local = "127.0.0.1:3100".parse().unwrap();
        let mut client = session::Session::new(&config, local).unwrap();
        let mut peer = Endpoint::new(
            config.peer,
            TransportProtocol::UDP,
            Some(Arc::new(server_config)),
        );
        client.start(Instant::now()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut server_complete = false;
        for _ in 0..64 {
            assert!(
                Instant::now() < deadline,
                "Rust-only handshake exceeded watchdog"
            );
            while let Some(packet) = client.poll_transmit() {
                for event in peer
                    .read(
                        Instant::now(),
                        local,
                        None,
                        BytesMut::from(packet.as_slice()),
                    )
                    .unwrap()
                {
                    server_complete |= matches!(event, EndpointEvent::HandshakeComplete);
                }
            }
            while let Some(packet) = peer.poll_transmit() {
                client
                    .read(Instant::now(), packet.message.to_vec())
                    .unwrap();
            }
            if client.connected() && server_complete {
                break;
            }
        }
        assert!(client.connected() && server_complete);
        assert_eq!(&*observed.lock().unwrap(), b"id\0opaque");
        client
            .write(Instant::now(), b"owned opaque identity")
            .unwrap();
        let packet = client.poll_transmit().unwrap();
        let events = peer
            .read(
                Instant::now(),
                local,
                None,
                BytesMut::from(packet.as_slice()),
            )
            .unwrap();
        assert!(events.into_iter().any(|event| matches!(event,
            EndpointEvent::ApplicationData(data) if data.as_ref() == b"owned opaque identity")));
        // Larger UDP datagrams can be filtered by local test sandboxes. The
        // Rust-only peer also proves the full encoder bound without a wire cap.
        let layout = HueLayout::new(
            *b"12345678-1234-1234-1234-123456789abc",
            (0..=255).collect(),
            0,
        )
        .unwrap();
        let payload = layout.pack(&[1, 2, 3].repeat(256));
        assert_eq!(payload.len(), 1844);
        assert_eq!(
            &payload[..52],
            b"HueStream\x02\0\0\0\0\0\0"
                .iter()
                .chain(b"12345678-1234-1234-1234-123456789abc")
                .copied()
                .collect::<Vec<_>>()
        );
        assert_eq!(&payload[52..59], &[0, 1, 1, 2, 2, 3, 3]);
        assert_eq!(&payload[1837..], &[255, 1, 1, 2, 2, 3, 3]);
        client.write(Instant::now(), &payload).unwrap();
        let packet = client.poll_transmit().unwrap();
        assert_eq!(packet.len(), 1881);
        let events = peer
            .read(
                Instant::now(),
                local,
                None,
                BytesMut::from(packet.as_slice()),
            )
            .unwrap();
        assert!(events.into_iter().any(|event| matches!(event,
            EndpointEvent::ApplicationData(data) if data.as_ref() == payload)));
    }

    #[test]
    fn close_publication_cannot_be_overwritten_by_handshake_completion() {
        let engine = HueEngine::build(
            "127.0.0.1",
            2100,
            b"id",
            &[0; 16],
            b"12345678-1234-1234-1234-123456789abc",
            &[7],
            0,
            5.0,
            0.2,
            0.2,
        )
        .unwrap();
        engine.state.store(CONNECTING, Ordering::Release);
        engine.state.store(CLOSED, Ordering::Release);
        engine.cancel.cancel();
        assert!(engine.publish_connected().is_err());
        assert_eq!(engine.state.load(Ordering::Acquire), CLOSED);
        let mut client = None;
        assert!(engine.finish(&mut client, Ok(())).is_err());
        assert_eq!(engine.state.load(Ordering::Acquire), CLOSED);
        assert!(engine.config.lock().unwrap().is_none());
    }

    #[test]
    fn native_lock_wait_is_cancelled_and_close_lock_ignores_own_cancellation() {
        use std::{sync::mpsc, thread};
        let engine = Arc::new(
            HueEngine::build(
                "127.0.0.1",
                2100,
                b"id",
                &[0; 16],
                b"12345678-1234-1234-1234-123456789abc",
                &[7],
                0,
                5.0,
                0.2,
                0.2,
            )
            .unwrap(),
        );
        let held = engine.inner.lock().unwrap();
        let worker_engine = Arc::clone(&engine);
        let (started_tx, started_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result = worker_engine.lock(Instant::now() + Duration::from_secs(5), false);
            result_tx
                .send(matches!(result, Err(HueError::Closed)))
                .unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        engine.cancel.cancel();
        let outcome = result_rx.recv_timeout(Duration::from_millis(200));
        drop(held);
        worker.join().unwrap();
        assert!(outcome.unwrap(), "lock waiter ignored cancellation");
        assert!(
            engine
                .lock(Instant::now() + Duration::from_millis(200), true)
                .is_ok()
        );
    }
    #[test]
    fn expired_send_budget_fails_and_releases_unconnected_credentials() {
        let engine = HueEngine::build(
            "127.0.0.1",
            2100,
            b"id",
            &[0; 16],
            b"12345678-1234-1234-1234-123456789abc",
            &[7],
            0,
            5.0,
            0.2,
            0.2,
        )
        .unwrap();
        assert!(matches!(
            engine.send_owned(vec![0; 59], Instant::now()),
            Err(HueError::Timeout)
        ));
        assert_eq!(engine.state.load(Ordering::Acquire), FAILED);
        assert!(engine.cancel.cancelled());
        assert!(engine.config.lock().unwrap().is_none());
    }
}
