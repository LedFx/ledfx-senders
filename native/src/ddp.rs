//! Whole-frame DDP/OPC engine sharing owned banks and deadline UDP transport.
use crate::buffer::{Banks, NumericPolicy};
use crate::sender::Transport;
use crate::transport::{Datagram, DatagramTransport, MemoryTransport, SocketTransport};
use pyo3::{
    exceptions::{PyOSError, PyOverflowError, PyRuntimeError, PyValueError},
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

fn banks(count: usize, id: u8) -> Result<Banks, &'static str> {
    if count == 0 || count > u32::MAX as usize || id == 0 {
        return Err("invalid DDP channel count or destination ID");
    }
    let mut templates = Vec::new();
    let mut spans = Vec::new();
    for start in (0..count).step_by(1440) {
        let len = (count - start).min(1440);
        let mut p = vec![0; 10 + len];
        p[0] = if start + len == count { 0x41 } else { 0x40 };
        p[2] = 11;
        p[3] = id;
        p[4..8].copy_from_slice(&(start as u32).to_be_bytes());
        p[8..10].copy_from_slice(&(len as u16).to_be_bytes());
        spans.push((templates.len(), start, 0, len));
        templates.push(p);
    }
    Banks::with_policy(templates, spans, count, 10, NumericPolicy::Wrap)
}

struct State<T: DatagramTransport> {
    banks: Banks,
    transport: T,
    destination: SocketAddrV4,
    ddp: bool,
    sequence: u8,
    closed: bool,
    datagrams: u64,
    bytes: u64,
    errors: u64,
}
impl<T: DatagramTransport> State<T> {
    fn send(&mut self, kind: u8) -> io::Result<()> {
        self.banks
            .prepare(kind)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        if self.ddp {
            self.sequence = self.sequence % 15 + 1;
            for packet in &mut self.banks.staging {
                packet[1] = self.sequence;
            }
        }
        let deadline = Instant::now() + Duration::from_millis(200);
        // Fixed stack descriptors avoid an allocation per frame.
        let mut accepted = 0;
        for chunk in self.banks.staging.chunks(1024) {
            let mut packets: [Datagram<'_>; 1024] = std::array::from_fn(|_| Datagram {
                bytes: &[],
                destination: self.destination,
            });
            for (descriptor, bytes) in packets.iter_mut().zip(chunk) {
                descriptor.bytes = bytes;
            }
            let result = self.transport.send_batch(&packets[..chunk.len()], deadline);
            match result {
                Ok(n) => {
                    accepted += n;
                    self.datagrams += n as u64;
                    self.bytes += chunk[..n].iter().map(|p| p.len() as u64).sum::<u64>();
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
        if accepted != self.banks.staging.len() {
            self.errors += 1;
            return Err(io::Error::new(io::ErrorKind::TimedOut, "partial UDP frame"));
        }
        self.banks.commit();
        Ok(())
    }
}

#[pyclass]
pub struct PacketEngine {
    inner: Mutex<State<Transport>>,
}
#[pymethods]
impl PacketEngine {
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

    #[new]
    #[pyo3(signature=(protocol,count,destination,identifier,mode="socket",backend="batched",batch_size=64,override_destination=None))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        protocol: &str,
        count: usize,
        destination: &str,
        identifier: u8,
        mode: &str,
        backend: &str,
        batch_size: usize,
        override_destination: Option<&str>,
    ) -> PyResult<Self> {
        let banks = match protocol {
            "ddp" => banks(count, identifier),
            "opc" => crate::opc::banks(count, identifier),
            _ => return Err(PyValueError::new_err("invalid protocol")),
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
            inner: Mutex::new(State {
                banks,
                transport,
                destination,
                ddp: protocol == "ddp",
                sequence: 1,
                closed: false,
                datagrams: 0,
                bytes: 0,
                errors: 0,
            }),
        })
    }
    fn send(&self, py: Python<'_>, frame: &Bound<'_, PyAny>) -> PyResult<()> {
        let view = PyMemoryView::from(frame)?;
        let result = {
            let mut s = self
                .inner
                .lock_py_attached(py)
                .map_err(|_| PyOSError::new_err("engine lock poisoned"))?;
            if s.closed {
                return Err(PyRuntimeError::new_err("sender closed"));
            }
            let kind = s.banks.snapshot_input(py, &view)?;
            let s = &mut *s;
            py.detach(|| s.send(kind))
        };
        drop(view);
        result.map_err(|e| {
            if e.to_string() == "infinite channel level" {
                PyOverflowError::new_err(e.to_string())
            } else if e.kind() == io::ErrorKind::InvalidInput {
                PyValueError::new_err(e.to_string())
            } else {
                PyOSError::new_err(e)
            }
        })
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
        Ok((s.datagrams, s.bytes, s.errors))
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
            .banks
            .committed
            .clone();
        Ok(packets
            .iter()
            .map(|p| PyBytes::new(py, p).unbind())
            .collect())
    }
}

#[cfg(test)]
#[path = "../tests/ddp.rs"]
mod tests;
