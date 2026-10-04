//! Nanoleaf's single-datagram UDP output. REST/HTTP session control stays outside.
use crate::{
    buffer::{Banks, NumericPolicy},
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

fn banks(version: u8, ids: &[u16]) -> Result<Banks, &'static str> {
    let limit = match version {
        1 => 255,
        2 => 8188,
        _ => return Err("invalid Nanoleaf version"),
    };
    if ids.is_empty()
        || ids.len() > limit
        || (version == 1 && ids.iter().any(|id| *id > 255))
        || ids.iter().enumerate().any(|(i, id)| ids[..i].contains(id))
    {
        return Err("invalid Nanoleaf panel layout");
    }
    let mut packet = if version == 1 {
        vec![ids.len() as u8]
    } else {
        (ids.len() as u16).to_be_bytes().to_vec()
    };
    let mut spans = Vec::with_capacity(ids.len());
    for (i, id) in ids.iter().enumerate() {
        let rgb = packet.len() + 2;
        if version == 1 {
            packet.extend_from_slice(&[*id as u8, 1, 0, 0, 0, 0, 1]);
        } else {
            packet.extend_from_slice(&id.to_be_bytes());
            packet.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        }
        spans.push((0, i * 3, rgb, 3));
    }
    Banks::with_policy(vec![packet], spans, ids.len() * 3, 0, NumericPolicy::Clip)
}

struct State<T: DatagramTransport> {
    banks: Banks,
    transport: T,
    destination: SocketAddrV4,
    closed: bool,
    counters: (u64, u64, u64),
}
impl<T: DatagramTransport> State<T> {
    fn send(&mut self, kind: u8) -> io::Result<()> {
        self.banks
            .prepare(kind)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let packet = &self.banks.staging[0];
        match self.transport.send_batch(
            &[Datagram {
                bytes: packet,
                destination: self.destination,
            }],
            Instant::now() + Duration::from_millis(200),
        ) {
            Ok(1) => {
                self.counters.0 += 1;
                self.counters.1 += packet.len() as u64;
                self.banks.commit();
                Ok(())
            }
            Ok(_) => {
                self.counters.2 += 1;
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Nanoleaf datagram not accepted",
                ))
            }
            Err(e) => {
                self.counters.2 += 1;
                Err(e)
            }
        }
    }
}

#[pyclass]
pub struct NanoleafEngine {
    inner: Mutex<State<Transport>>,
}
#[pymethods]
impl NanoleafEngine {
    #[new]
    #[pyo3(signature=(destination, version, ids, mode="socket"))]
    fn new(destination: &str, version: u8, ids: Vec<u16>, mode: &str) -> PyResult<Self> {
        let banks = banks(version, &ids).map_err(PyValueError::new_err)?;
        let destination: SocketAddrV4 = destination
            .parse()
            .map_err(|_| PyValueError::new_err("invalid IPv4 destination"))?;
        if destination.port() == 0 {
            return Err(PyValueError::new_err("invalid port"));
        }
        let transport = match mode {
            "socket" => {
                Transport::Socket(SocketTransport::new(false, 1).map_err(PyOSError::new_err)?)
            }
            "capture" | "discard" => Transport::Memory(MemoryTransport {
                capture: mode == "capture",
                packets: Vec::new(),
            }),
            _ => return Err(PyValueError::new_err("invalid transport mode")),
        };
        Ok(Self {
            inner: Mutex::new(State {
                banks,
                transport,
                destination,
                closed: false,
                counters: (0, 0, 0),
            }),
        })
    }
    fn send(&self, py: Python<'_>, frame: &Bound<'_, PyAny>) -> PyResult<()> {
        let view = PyMemoryView::from(frame)?;
        let result = {
            let mut state = self
                .inner
                .lock_py_attached(py)
                .map_err(|_| PyRuntimeError::new_err("engine lock poisoned"))?;
            if state.closed {
                return Err(PyRuntimeError::new_err("sender closed"));
            }
            let kind = state.banks.snapshot_input(py, &view)?;
            let state = &mut *state;
            py.detach(|| state.send(kind))
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
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        let mut state = self
            .inner
            .lock_py_attached(py)
            .map_err(|_| PyRuntimeError::new_err("engine lock poisoned"))?;
        let state = &mut *state;
        py.detach(|| {
            state.transport.close();
            state.closed = true;
        });
        Ok(())
    }
    #[getter]
    fn closed(&self, py: Python<'_>) -> PyResult<bool> {
        Ok(self
            .inner
            .lock_py_attached(py)
            .map_err(|_| PyRuntimeError::new_err("engine lock poisoned"))?
            .closed)
    }
    fn counters(&self, py: Python<'_>) -> PyResult<(u64, u64, u64)> {
        Ok(self
            .inner
            .lock_py_attached(py)
            .map_err(|_| PyRuntimeError::new_err("engine lock poisoned"))?
            .counters)
    }
    fn transport_info(&self, py: Python<'_>) -> PyResult<(String, usize, u64, u64)> {
        let state = self
            .inner
            .lock_py_attached(py)
            .map_err(|_| PyRuntimeError::new_err("engine lock poisoned"))?;
        Ok(match &state.transport {
            Transport::Socket(t) => (
                "portable".to_owned(),
                1,
                t.syscalls.get(),
                t.readiness_waits.get(),
            ),
            Transport::Memory(t) => (
                if t.capture { "capture" } else { "discard" }.to_owned(),
                1,
                0,
                0,
            ),
        })
    }
    fn captures(&self, py: Python<'_>) -> PyResult<Vec<(Py<PyBytes>, String)>> {
        let packets = {
            let state = self
                .inner
                .lock_py_attached(py)
                .map_err(|_| PyRuntimeError::new_err("engine lock poisoned"))?;
            match &state.transport {
                Transport::Memory(t) => t.packets.clone(),
                _ => Vec::new(),
            }
        };
        Ok(packets
            .into_iter()
            .map(|(p, a)| (PyBytes::new(py, &p).unbind(), a))
            .collect())
    }
    fn committed_copy(&self, py: Python<'_>) -> PyResult<Vec<Py<PyBytes>>> {
        let packets = self
            .inner
            .lock_py_attached(py)
            .map_err(|_| PyRuntimeError::new_err("engine lock poisoned"))?
            .banks
            .committed
            .clone();
        Ok(packets
            .iter()
            .map(|p| PyBytes::new(py, p).unbind())
            .collect())
    }
    fn _test_hold_lock(
        &self,
        py: Python<'_>,
        gate: &crate::test_gate::TestLockGate,
    ) -> PyResult<()> {
        let shared = std::sync::Arc::clone(&gate.shared);
        py.detach(|| {
            let guard = self
                .inner
                .try_lock()
                .map_err(|_| "engine must be idle before test gate")?;
            let result = shared.hold();
            drop(guard);
            result
        })
        .map_err(PyRuntimeError::new_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Refuse;
    impl DatagramTransport for Refuse {
        fn send_batch(&mut self, _: &[Datagram<'_>], deadline: Instant) -> io::Result<usize> {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(left <= Duration::from_millis(200) && left > Duration::ZERO);
            Ok(0)
        }
        fn close(&mut self) {}
    }
    #[test]
    fn rejected_datagram_does_not_commit_or_count_acceptance() {
        let mut state = State {
            banks: banks(1, &[7]).unwrap(),
            transport: Refuse,
            destination: "127.0.0.1:60222".parse().unwrap(),
            closed: false,
            counters: (0, 0, 0),
        };
        let committed = state.banks.committed.clone();
        assert_eq!(state.send(0).unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_eq!(state.counters, (0, 0, 1));
        assert_eq!(state.banks.committed, committed);
    }
}
