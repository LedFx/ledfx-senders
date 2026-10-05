//! Native packet engine for the independent ledfx-senders distribution.
pub mod artnet;
mod artnet_numeric;
pub mod buffer;
mod change_mask;
mod convert;
pub mod ddp;
mod encoders;
mod hue;
mod nanoleaf;
pub mod opc;
mod original;
mod osc;
mod osc_numeric;
mod realtime;
pub mod sender;
mod test_gate;
pub mod transport;
use crate::buffer::Banks;
use crate::sender::{Sender, Transport};
use crate::transport::{MemoryTransport, SocketTransport};
use pyo3::types::{PyDict, PyMemoryView};
use std::io;
use std::net::SocketAddrV4;

#[pyfunction]
fn engine_info(py: Python<'_>) -> PyResult<Bound<'_, PyDict>> {
    let info = PyDict::new(py);
    info.set_item("engine", "ledfx-e131")?;
    info.set_item("version", env!("CARGO_PKG_VERSION"))?;
    info.set_item("binding", "pyo3-0.29.2")?;
    info.set_item("compiler", env!("LEDFX_RUSTC_VERSION"))?;
    info.set_item("profile", env!("LEDFX_BUILD_PROFILE"))?;
    Ok(info)
}

// Every mutable bank, sequence and transport is behind the per-engine mutex.
// Exporter callbacks occur outside it; lock_py_attached cooperates with GC.
#[pymodule(gil_used = false)]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<buffer::PacketBanks>()?;
    module.add_class::<Engine>()?;
    module.add_class::<ddp::PacketEngine>()?;
    module.add_class::<artnet::ArtNetEngine>()?;
    module.add_class::<realtime::StatefulEngine>()?;
    module.add_class::<test_gate::TestLockGate>()?;
    module.add_class::<nanoleaf::NanoleafEngine>()?;
    module.add_class::<encoders::RGBGather>()?;
    module.add_function(wrap_pyfunction!(engine_info, module)?)?;
    module.add_function(wrap_pyfunction!(encoders::encode_adalight, module)?)?;
    module.add_function(wrap_pyfunction!(encoders::encode_openrgb, module)?)?;
    module.add_function(wrap_pyfunction!(encoders::encode_hue, module)?)?;
    module.add_function(wrap_pyfunction!(encoders::encode_govee, module)?)?;
    Ok(())
}

use pyo3::{
    exceptions::{PyOSError, PyRuntimeError, PyValueError},
    prelude::*,
    sync::MutexExt,
};
use std::sync::Mutex;
#[pyclass]
pub struct Engine {
    inner: Mutex<Sender<Transport>>,
}
// Mirrors setup-only binding arguments; grouping would obscure the public ABI.
#[allow(clippy::too_many_arguments)]
fn construct(
    templates: Vec<Vec<u8>>,
    spans: Vec<crate::buffer::Span>,
    count: usize,
    destinations: Vec<String>,
    sync: Vec<u8>,
    discovery: Vec<Vec<u8>>,
    mode: &str,
    backend: &str,
    batch_size: usize,
    override_destination: Option<String>,
) -> PyResult<Engine> {
    let remap: Option<SocketAddrV4> = override_destination
        .map(|s| {
            s.parse()
                .map_err(|_| PyValueError::new_err("invalid override destination"))
        })
        .transpose()?;
    if remap.is_some_and(|a| !a.ip().is_loopback()) {
        return Err(PyValueError::new_err("override must be loopback"));
    }
    let destinations = destinations
        .into_iter()
        .map(|s| {
            s.parse()
                .map_err(|_| PyValueError::new_err("invalid IPv4 destination"))
        })
        .collect::<PyResult<Vec<SocketAddrV4>>>()?;
    let destinations = if let Some(a) = remap {
        vec![a; destinations.len()]
    } else {
        destinations
    };
    if batch_size == 0 || batch_size > 1024 {
        return Err(PyValueError::new_err("invalid batch size"));
    }
    if backend != "portable" && backend != "batched" {
        return Err(PyValueError::new_err("invalid backend"));
    }
    let transport = match mode {
        "socket" => Transport::Socket(
            SocketTransport::new(backend == "batched", batch_size).map_err(PyOSError::new_err)?,
        ),
        "capture" | "discard" => Transport::Memory(MemoryTransport {
            capture: mode == "capture",
            packets: Vec::new(),
        }),
        _ => return Err(PyValueError::new_err("invalid transport mode")),
    };
    let banks = Banks::new(templates, spans, count).map_err(PyValueError::new_err)?;
    Ok(Engine {
        inner: Mutex::new(
            Sender::new(banks, transport, destinations, sync, discovery, remap)
                .map_err(PyOSError::new_err)?,
        ),
    })
}
#[pymethods]
impl Engine {
    fn _test_hold_lock(&self, py: Python<'_>, gate: &test_gate::TestLockGate) -> PyResult<()> {
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

    fn _test_loopback_multicast(&self, py: Python<'_>) -> PyResult<()> {
        let sender = self
            .inner
            .lock_py_attached(py)
            .map_err(|_| PyOSError::new_err("engine lock poisoned"))?;
        match &sender.transport {
            Transport::Socket(transport) => {
                transport.loopback_multicast().map_err(PyOSError::new_err)
            }
            _ => Err(PyValueError::new_err("requires socket transport")),
        }
    }

    #[new]
    fn new(
        templates: Vec<Vec<u8>>,
        spans: Vec<crate::buffer::Span>,
        count: usize,
        destinations: Vec<String>,
        sync: Vec<u8>,
        discovery: Vec<Vec<u8>>,
    ) -> PyResult<Self> {
        construct(
            templates,
            spans,
            count,
            destinations,
            sync,
            discovery,
            "socket",
            "batched",
            64,
            None,
        )
    }
    #[staticmethod]
    #[pyo3(signature=(templates,spans,count,destinations,sync,discovery,mode,backend="batched",batch_size=64,override_destination=None))]
    // Explicit internal construction controls are intentionally keyword-visible.
    #[allow(clippy::too_many_arguments)]
    fn _test_engine(
        templates: Vec<Vec<u8>>,
        spans: Vec<crate::buffer::Span>,
        count: usize,
        destinations: Vec<String>,
        sync: Vec<u8>,
        discovery: Vec<Vec<u8>>,
        mode: &str,
        backend: &str,
        batch_size: usize,
        override_destination: Option<String>,
    ) -> PyResult<Self> {
        construct(
            templates,
            spans,
            count,
            destinations,
            sync,
            discovery,
            mode,
            backend,
            batch_size,
            override_destination,
        )
    }
    fn send(&self, py: Python<'_>, frame: &Bound<'_, PyAny>, now: f64) -> PyResult<()> {
        // Exporter callbacks run here, before any engine lock. Keep this owning
        // built-in view alive until after the guard drops, so typed PyBuffer
        // release during snapshot cannot release the original caller export.
        let view = PyMemoryView::from(frame)?;
        let result = {
            let mut guard = self
                .inner
                .lock_py_attached(py)
                .map_err(|_| PyOSError::new_err("engine lock poisoned"))?;
            if guard.closed {
                drop(guard);
                return Err(PyRuntimeError::new_err("sender closed"));
            }
            let kind = guard.banks.snapshot_input(py, &view)?;
            let sender = &mut *guard;
            py.detach(|| sender.send_prepared(kind, now))
        };
        drop(view); // Attached exporter release may re-enter; mutex is free.
        result.map_err(|e| {
            if e.kind() == io::ErrorKind::InvalidInput {
                PyValueError::new_err(e.to_string())
            } else {
                PyOSError::new_err(e)
            }
        })
    }
    fn service(&self, py: Python<'_>, now: f64) -> PyResult<()> {
        let result = {
            let mut guard = self
                .inner
                .lock_py_attached(py)
                .map_err(|_| PyOSError::new_err("engine lock poisoned"))?;
            let sender = &mut *guard;
            py.detach(|| sender.service(now))
        };
        result.map_err(PyOSError::new_err)
    }
    fn close(&self, py: Python<'_>, blackout: bool, now: f64) -> PyResult<()> {
        let mut guard = self
            .inner
            .lock_py_attached(py)
            .map_err(|_| PyOSError::new_err("engine lock poisoned"))?;
        let sender = &mut *guard;
        py.detach(|| sender.close(blackout, now));
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
    /// Backend, configured chunk size, syscall submissions and readiness waits.
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
    fn cleanup_error(&self, py: Python<'_>) -> PyResult<Option<String>> {
        Ok(self
            .inner
            .lock_py_attached(py)
            .map_err(|_| PyOSError::new_err("engine lock poisoned"))?
            .cleanup_error
            .clone())
    }
    fn captures(&self, py: Python<'_>) -> PyResult<Vec<(Py<pyo3::types::PyBytes>, String)>> {
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
            .map(|(p, d)| (pyo3::types::PyBytes::new(py, &p).unbind(), d))
            .collect())
    }
    fn committed_copy(&self, py: Python<'_>) -> PyResult<Vec<Py<pyo3::types::PyBytes>>> {
        let packets = {
            self.inner
                .lock_py_attached(py)
                .map_err(|_| PyOSError::new_err("engine lock poisoned"))?
                .banks
                .committed
                .clone()
        };
        Ok(packets
            .iter()
            .map(|p| pyo3::types::PyBytes::new(py, p).unbind())
            .collect())
    }
}
