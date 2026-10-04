//! Stateful OSC and WLED realtime submission over the shared deadline transport.
use crate::{
    original::Original,
    osc,
    sender::Transport,
    transport::{Datagram, DatagramTransport, MemoryTransport, SocketTransport},
};
use pyo3::{
    exceptions::{PyOSError, PyRuntimeError, PyValueError},
    prelude::*,
    sync::MutexExt,
    types::{PyBytes, PyMemoryView},
};
use std::{
    io,
    net::SocketAddrV4,
    sync::Mutex,
    time::{Duration, Instant},
};

enum Layout {
    Osc(osc::Layout),
    Realtime {
        kind: u8,
        adaptive: bool,
        timeout: u8,
        packets: Vec<Vec<u8>>,
    },
}
impl Layout {
    fn realtime(pixels: usize, mode: &str, timeout: u8) -> Result<Self, &'static str> {
        if pixels > 65536 || timeout == 0 {
            return Err("invalid realtime pixel count or timeout");
        }
        let kind = match mode {
            "DRGB" if pixels <= 490 => 2,
            "WARLS" if pixels <= 255 => 1,
            "DRGBW" if pixels <= 367 => 3,
            "DNRGB" => 4,
            "adaptive_smallest" if pixels <= 255 => 2,
            "RGB (HyperHDR)" if pixels <= 500 => 0,
            "DRGB" | "WARLS" | "DRGBW" | "adaptive_smallest" | "RGB (HyperHDR)" => {
                if pixels <= 490 { 2 } else { 4 }
            }
            _ => return Err("invalid realtime packet type"),
        };
        let packets = if kind == 4 {
            (0..pixels)
                .step_by(489)
                .map(|start| vec![0; 4 + (pixels - start).min(489) * 3])
                .collect()
        } else {
            vec![Vec::with_capacity(2 + pixels * 4)]
        };
        Ok(Self::Realtime {
            kind,
            adaptive: mode == "adaptive_smallest" && pixels <= 255,
            timeout,
            packets,
        })
    }
    fn is_osc(&self) -> bool {
        matches!(self, Self::Osc(_))
    }
    fn packets(&self) -> &[Vec<u8>] {
        match self {
            Self::Osc(l) => &l.packets,
            Self::Realtime { packets, .. } => packets,
        }
    }
    fn pack(&mut self, bytes: &[u8], floats: &[f32], mask: &[bool], changed: usize) {
        match self {
            Self::Osc(l) => l.pack(floats),
            Self::Realtime {
                kind,
                adaptive,
                timeout,
                packets,
            } => {
                let kind = if *adaptive {
                    if changed * 4 < mask.len() * 3 { 1 } else { 2 }
                } else {
                    *kind
                };
                if kind == 4 {
                    for (index, (packet, payload)) in
                        packets.iter_mut().zip(bytes.chunks(489 * 3)).enumerate()
                    {
                        packet[0] = 4;
                        packet[1] = *timeout;
                        packet[2..4].copy_from_slice(&((index * 489) as u16).to_be_bytes());
                        packet[4..].copy_from_slice(payload);
                    }
                } else {
                    let packet = &mut packets[0];
                    packet.clear();
                    if kind != 0 {
                        packet.extend_from_slice(&[kind, *timeout])
                    }
                    match kind {
                        0 | 2 => packet.extend_from_slice(bytes),
                        1 => {
                            for (i, (rgb, &changed)) in bytes.chunks_exact(3).zip(mask).enumerate()
                            {
                                if changed {
                                    packet.push(i as u8);
                                    packet.extend_from_slice(rgb)
                                }
                            }
                        }
                        3 => {
                            for rgb in bytes.chunks_exact(3) {
                                packet.extend_from_slice(rgb);
                                packet.push(0)
                            }
                        }
                        _ => unreachable!(),
                    }
                }
            }
        }
    }
}
struct State<T: DatagramTransport> {
    current: Original,
    previous: Original,
    initialized: bool,
    bytes: Vec<u8>,
    floats: Vec<f32>,
    mask: Vec<bool>,
    layout: Layout,
    committed: Vec<Vec<u8>>,
    transport: T,
    destination: SocketAddrV4,
    minimise: bool,
    interval: f64,
    last: Option<f64>,
    closed: bool,
    datagrams: u64,
    wire_bytes: u64,
    errors: u64,
    attempts: u64,
    suppressed: u64,
    frames: u64,
}
impl<T: DatagramTransport> State<T> {
    fn send(&mut self, now: f64) -> io::Result<()> {
        if !now.is_finite() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "nonfinite timestamp",
            ));
        }
        // Equality to a successfully validated same-dtype common snapshot proves
        // OSC domain validity. Snapshot shape/export validation already ran under
        // the GIL. Rare/cross-dtype and realtime routes retain validation first.
        let typed_osc_change = if self.layout.is_osc()
            && self.initialized
            && self.current.kind <= 4
            && self.current.kind == self.previous.kind
        {
            Some(self.current.changed(&self.previous, true, &mut self.mask))
        } else {
            None
        };
        if typed_osc_change == Some(0) {
            self.attempts += 1;
            self.suppressed += 1;
            return Ok(());
        }
        self.current
            .encode(self.layout.is_osc(), &mut self.bytes, &mut self.floats)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let changed = typed_osc_change.unwrap_or_else(|| {
            self.current
                .changed(&self.previous, self.initialized, &mut self.mask)
        });
        self.attempts += 1;
        if changed == 0
            && (self.layout.is_osc()
                || self.minimise && self.last.is_some_and(|last| now <= last + self.interval))
        {
            self.suppressed += 1;
            return Ok(());
        }
        self.layout
            .pack(&self.bytes, &self.floats, &self.mask, changed);
        let deadline = Instant::now() + Duration::from_millis(200);
        let mut accepted = 0;
        for chunk in self.layout.packets().chunks(1024) {
            let mut descriptors: [Datagram<'_>; 1024] = std::array::from_fn(|_| Datagram {
                bytes: &[],
                destination: self.destination,
            });
            for (d, p) in descriptors.iter_mut().zip(chunk) {
                d.bytes = p;
            }
            match self
                .transport
                .send_batch(&descriptors[..chunk.len()], deadline)
            {
                Ok(n) => {
                    accepted += n;
                    self.datagrams += n as u64;
                    self.wire_bytes += chunk[..n].iter().map(|p| p.len() as u64).sum::<u64>();
                    if n != chunk.len() {
                        break;
                    }
                }
                Err(e) => {
                    self.errors += 1;
                    return Err(e);
                }
            }
        }
        if accepted != self.layout.packets().len() {
            self.errors += 1;
            return Err(io::Error::new(io::ErrorKind::TimedOut, "partial UDP frame"));
        }
        for (out, packet) in self.committed.iter_mut().zip(self.layout.packets()) {
            out.clone_from(packet)
        }
        std::mem::swap(&mut self.current, &mut self.previous);
        self.initialized = true;
        self.last = Some(now);
        self.frames += 1;
        Ok(())
    }
}
#[pyclass]
pub struct StatefulEngine {
    inner: Mutex<State<Transport>>,
    count: usize,
}
#[pymethods]
impl StatefulEngine {
    #[new]
    #[pyo3(signature=(protocol,pixels,destination,mode_name,paths,timeout,minimise,interval,mode="socket",backend="batched",batch_size=64,override_destination=None))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        protocol: &str,
        pixels: usize,
        destination: &str,
        mode_name: &str,
        paths: Vec<Vec<u8>>,
        timeout: u8,
        minimise: bool,
        interval: f64,
        mode: &str,
        backend: &str,
        batch_size: usize,
        override_destination: Option<&str>,
    ) -> PyResult<Self> {
        if pixels == 0 || pixels > 1_000_000 || !interval.is_finite() || interval < 0.0 {
            return Err(PyValueError::new_err("invalid pixel count or interval"));
        }
        let count = pixels * 3;
        let layout = match protocol {
            "osc" => osc::Layout::new(count, mode_name, paths).map(Layout::Osc),
            "realtime" => Layout::realtime(pixels, mode_name, timeout),
            _ => Err("invalid protocol"),
        }
        .map_err(PyValueError::new_err)?;
        let mut destination: SocketAddrV4 = destination
            .parse()
            .map_err(|_| PyValueError::new_err("invalid IPv4 destination"))?;
        if let Some(address) = override_destination {
            destination = address
                .parse()
                .map_err(|_| PyValueError::new_err("invalid override"))?;
            if !destination.ip().is_loopback() {
                return Err(PyValueError::new_err("override must be loopback"));
            }
        }
        if destination.port() == 0
            || !["portable", "batched"].contains(&backend)
            || !(1..=1024).contains(&batch_size)
        {
            return Err(PyValueError::new_err("invalid transport settings"));
        }
        let transport = match mode {
            "socket" => Transport::Socket(
                SocketTransport::new(backend == "batched", batch_size)
                    .map_err(PyOSError::new_err)?,
            ),
            "capture" | "discard" => Transport::Memory(MemoryTransport {
                capture: mode == "capture",
                packets: Vec::new(),
            }),
            _ => return Err(PyValueError::new_err("invalid transport mode")),
        };
        Ok(Self {
            count,
            inner: Mutex::new(State {
                current: Original::default(),
                previous: Original::default(),
                initialized: false,
                bytes: vec![0; count],
                floats: vec![0.0; count],
                mask: vec![false; pixels],
                committed: layout.packets().to_vec(),
                layout,
                transport,
                destination,
                minimise,
                interval,
                last: None,
                closed: false,
                datagrams: 0,
                wire_bytes: 0,
                errors: 0,
                attempts: 0,
                suppressed: 0,
                frames: 0,
            }),
        })
    }
    fn send(&self, py: Python<'_>, frame: &Bound<'_, PyAny>, kind: u8, now: f64) -> PyResult<()> {
        let view = PyMemoryView::from(frame)?;
        let result = {
            let mut state = self
                .inner
                .lock_py_attached(py)
                .map_err(|_| PyOSError::new_err("engine lock poisoned"))?;
            if state.closed {
                return Err(PyRuntimeError::new_err("sender closed"));
            }
            state.current.snapshot(py, &view, kind, self.count)?;
            let state = &mut *state;
            py.detach(|| state.send(now))
        };
        drop(view);
        result.map_err(|e| {
            if e.kind() == io::ErrorKind::InvalidInput {
                PyValueError::new_err(e.to_string())
            } else {
                PyOSError::new_err(e)
            }
        })
    }
    fn frame_counters(&self, py: Python<'_>) -> PyResult<(u64, u64, u64)> {
        let s = self
            .inner
            .lock_py_attached(py)
            .map_err(|_| PyOSError::new_err("engine lock poisoned"))?;
        Ok((s.attempts, s.suppressed, s.frames))
    }
    fn _test_hold_lock(
        &self,
        py: Python<'_>,
        gate: &crate::test_gate::TestLockGate,
    ) -> PyResult<()> {
        let shared = std::sync::Arc::clone(&gate.shared);
        // Only Rust state is used detached. Drop the real engine guard before
        // reattaching even on timeout, so a broken GIL-held waiter cannot wedge
        // the diagnostic helper itself. No production path checks this gate.
        py.detach(|| {
            let guard = self
                .inner
                .try_lock()
                .map_err(|_| "engine must be idle before installing the test gate")?;
            let result = shared.hold();
            drop(guard);
            result
        })
        .map_err(PyRuntimeError::new_err)
    }

    fn close(&self, py: Python<'_>) -> PyResult<()> {
        let mut s = self
            .inner
            .lock_py_attached(py)
            .map_err(|_| PyOSError::new_err("engine lock poisoned"))?;
        let s = &mut *s;
        py.detach(|| {
            s.transport.close();
            s.closed = true;
        });
        Ok(())
    }
    #[getter]
    fn closed(&self, py: Python<'_>) -> PyResult<bool> {
        Ok(self
            .inner
            .lock_py_attached(py)
            .map_err(|_| PyOSError::new_err("engine lock poisoned"))?
            .closed)
    }
    fn counters(&self, py: Python<'_>) -> PyResult<(u64, u64, u64)> {
        let s = self
            .inner
            .lock_py_attached(py)
            .map_err(|_| PyOSError::new_err("engine lock poisoned"))?;
        Ok((s.datagrams, s.wire_bytes, s.errors))
    }
    fn captures(&self, py: Python<'_>) -> PyResult<Vec<(Py<PyBytes>, String)>> {
        let packets = {
            let s = self
                .inner
                .lock_py_attached(py)
                .map_err(|_| PyOSError::new_err("engine lock poisoned"))?;
            match &s.transport {
                Transport::Memory(t) => t.packets.clone(),
                _ => Vec::new(),
            }
        };
        Ok(packets
            .into_iter()
            .map(|(p, d)| (PyBytes::new(py, &p).unbind(), d))
            .collect())
    }
    fn transport_info(&self, py: Python<'_>) -> PyResult<(String, usize, u64, u64)> {
        let s = self
            .inner
            .lock_py_attached(py)
            .map_err(|_| PyOSError::new_err("engine lock poisoned"))?;
        Ok(match &s.transport {
            Transport::Socket(t) => (
                if t.batched && cfg!(target_os = "linux") {
                    "batched"
                } else {
                    "portable"
                }
                .to_owned(),
                t.batch_size,
                t.syscalls.get(),
                t.readiness_waits.get(),
            ),
            Transport::Memory(t) => (
                if t.capture { "capture" } else { "discard" }.to_owned(),
                0,
                0,
                0,
            ),
        })
    }
    fn committed_copy(&self, py: Python<'_>) -> PyResult<Vec<Py<PyBytes>>> {
        let packets = self
            .inner
            .lock_py_attached(py)
            .map_err(|_| PyOSError::new_err("engine lock poisoned"))?
            .committed
            .clone();
        Ok(packets
            .iter()
            .map(|p| PyBytes::new(py, p).unbind())
            .collect())
    }
}

#[cfg(test)]
#[path = "../tests/realtime.rs"]
mod tests;
