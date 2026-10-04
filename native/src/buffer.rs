//! Owned channel snapshots and two reusable packet banks. No transport policy.
use pyo3::buffer::PyBuffer;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::sync::MutexExt;
use pyo3::types::{PyBytes, PyMemoryView, PyTuple};
use std::sync::Mutex;

pub type Span = (usize, usize, usize, usize);

pub struct Banks {
    pub committed: Vec<Vec<u8>>,
    pub staging: Vec<Vec<u8>>,
    pub spans: Vec<Span>,
    bytes: Vec<u8>,
    floats: Vec<f32>,
    doubles: Vec<f64>,
    channels: Vec<u8>,
}

impl Banks {
    pub fn new(
        templates: Vec<Vec<u8>>,
        spans: Vec<Span>,
        count: usize,
    ) -> Result<Self, &'static str> {
        if count == 0 || templates.is_empty() || templates.iter().any(|p| p.len() != 638) {
            return Err("invalid templates or channel count");
        }
        let mut covered = 0;
        for &(packet, input, slot, len) in &spans {
            if packet >= templates.len()
                || input != covered
                || len == 0
                || slot.checked_add(len).is_none_or(|end| end > 512)
                || input.checked_add(len).is_none_or(|end| end > count)
            {
                return Err("invalid channel span");
            }
            covered += len;
        }
        if covered != count {
            return Err("incomplete channel spans");
        }
        Ok(Self {
            staging: templates.clone(),
            committed: templates,
            spans,
            bytes: vec![0; count],
            floats: vec![0.0; count],
            doubles: vec![0.0; count],
            channels: vec![0; count],
        })
    }

    /// Validate into scratch before touching staging, then scatter channels.
    /// Both banks preserve their original non-driven slots. Transport can stamp staging
    /// in place, send it by reference, and commit only after successful delivery.
    pub fn prepare(&mut self, kind: u8) -> Result<(), &'static str> {
        match kind {
            0 => {}
            1 => {
                // Accumulate validation without an early-exit dependency so LLVM
                // can vectorize validation. NaN and infinities fail these bounds.
                // Scratch may change on failure, but neither packet bank does.
                let mut invalid = false;
                for (target, &value) in self.channels.iter_mut().zip(&self.floats) {
                    invalid |= !((value > -1.0) & (value < 256.0));
                    *target = value as u8;
                }
                if invalid {
                    return Err("invalid channel level");
                }
            }
            2 => {
                // Accumulate validation without an early-exit dependency so LLVM
                // can vectorize validation. NaN and infinities fail these bounds.
                // Scratch may change on failure, but neither packet bank does.
                let mut invalid = false;
                for (target, &value) in self.channels.iter_mut().zip(&self.doubles) {
                    invalid |= !((value > -1.0) & (value < 256.0));
                    *target = value as u8;
                }
                if invalid {
                    return Err("invalid channel level");
                }
            }
            _ => return Err("unsupported snapshot kind"),
        }
        let channels = if kind == 0 {
            &self.bytes
        } else {
            &self.channels
        };
        for &(packet, input, slot, len) in &self.spans {
            self.staging[packet][126 + slot..126 + slot + len]
                .copy_from_slice(&channels[input..input + len]);
        }
        Ok(())
    }

    /// Copy once, attached, from an owning built-in memoryview acquired before
    /// locking. Typed exports of this view cannot call the original exporter:
    /// the owning view outlives all temporary PyBuffers, so their release cannot
    /// release its underlying export. No typed PyBuffer survives this method.
    pub fn snapshot_input(
        &mut self,
        py: Python<'_>,
        view: &Bound<'_, PyMemoryView>,
    ) -> PyResult<u8> {
        let frame = view.as_any();
        let kind;
        if let Ok(buffer) = PyBuffer::<u8>::get(frame) {
            if !buffer.is_c_contiguous() {
                return Err(PyValueError::new_err("noncontiguous frame"));
            }
            buffer.copy_to_slice(py, &mut self.bytes)?;
            kind = 0;
        } else if let Ok(buffer) = PyBuffer::<f32>::get(frame) {
            if !buffer.is_c_contiguous() {
                return Err(PyValueError::new_err("noncontiguous frame"));
            }
            buffer.copy_to_slice(py, &mut self.floats)?;
            kind = 1;
        } else {
            let buffer = PyBuffer::<f64>::get(frame)?;
            if !buffer.is_c_contiguous() {
                return Err(PyValueError::new_err("noncontiguous frame"));
            }
            buffer.copy_to_slice(py, &mut self.doubles)?;
            kind = 2;
        }
        Ok(kind)
    }

    #[cfg(test)]
    pub(crate) fn test_bytes(&mut self, bytes: &[u8]) {
        self.bytes.copy_from_slice(bytes);
    }

    pub fn commit(&mut self) {
        std::mem::swap(&mut self.committed, &mut self.staging);
    }
}

#[pyclass]
pub struct PacketBanks {
    pub inner: Mutex<Banks>,
}

#[pymethods]
impl PacketBanks {
    #[new]
    fn new(templates: Vec<Vec<u8>>, spans: Vec<Span>, count: usize) -> PyResult<Self> {
        Ok(Self {
            inner: Mutex::new(Banks::new(templates, spans, count).map_err(PyValueError::new_err)?),
        })
    }

    fn update(&self, py: Python<'_>, frame: &Bound<'_, PyAny>) -> PyResult<()> {
        // The owning envelope acquires/releases callback-capable exporters only
        // outside the bank mutex, including all early-return error paths.
        let view = PyMemoryView::from(frame)?;
        let result = {
            let mut guard = self
                .inner
                .lock_py_attached(py)
                .map_err(|_| PyValueError::new_err("buffer lock poisoned"))?;
            let kind = guard.snapshot_input(py, &view)?;
            let banks = &mut *guard;
            py.detach(|| {
                banks.prepare(kind)?;
                banks.commit();
                Ok::<(), &'static str>(())
            })
        };
        drop(view);
        result.map_err(PyValueError::new_err)
    }

    #[pyo3(signature = (staging=false))]
    fn snapshot<'py>(&self, py: Python<'py>, staging: bool) -> PyResult<Bound<'py, PyTuple>> {
        let packets = {
            let banks = self
                .inner
                .lock_py_attached(py)
                .map_err(|_| PyValueError::new_err("buffer lock poisoned"))?;
            if staging {
                banks.staging.clone()
            } else {
                banks.committed.clone()
            }
        };
        // Python allocation may run GC/finalizers which can re-enter this object.
        PyTuple::new(py, packets.iter().map(|packet| PyBytes::new(py, packet)))
    }

    fn capacities(&self, py: Python<'_>) -> PyResult<Vec<usize>> {
        let banks = self
            .inner
            .lock_py_attached(py)
            .map_err(|_| PyValueError::new_err("buffer lock poisoned"))?;
        let mut values = vec![
            banks.bytes.capacity(),
            banks.floats.capacity(),
            banks.doubles.capacity(),
            banks.channels.capacity(),
        ];
        values.extend(
            banks
                .committed
                .iter()
                .chain(&banks.staging)
                .map(Vec::capacity),
        );
        Ok(values)
    }
}

#[cfg(test)]
#[path = "../tests/buffer.rs"]
mod tests;

#[cfg(test)]
pub(crate) fn test_allocation_count() -> usize {
    tests::allocation_count()
}
