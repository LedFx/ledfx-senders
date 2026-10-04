//! Private, bounded diagnostic gate. Never consulted by production frame paths.
use pyo3::{exceptions::PyRuntimeError, prelude::*};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

#[derive(Default)]
struct State {
    entered: bool,
    released: bool,
}

#[derive(Default)]
pub(crate) struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    timed_out: AtomicBool,
}

impl Shared {
    fn wait_for(&self, predicate: impl Fn(&State) -> bool) -> Result<(), &'static str> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut state = self.state.lock().map_err(|_| "test gate poisoned")?;
        while !predicate(&state) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.timed_out.store(true, Ordering::SeqCst);
                return Err("test gate exceeded its five-second deadline");
            }
            state = self
                .changed
                .wait_timeout(state, remaining)
                .map_err(|_| "test gate poisoned")?
                .0;
        }
        Ok(())
    }

    pub(crate) fn hold(&self) -> Result<(), &'static str> {
        {
            let mut state = self.state.lock().map_err(|_| "test gate poisoned")?;
            state.entered = true;
            self.changed.notify_all();
        }
        self.wait_for(|state| state.released)
    }
}

#[pyclass(name = "_TestLockGate")]
#[derive(Default)]
pub struct TestLockGate {
    pub(crate) shared: Arc<Shared>,
}

#[pymethods]
impl TestLockGate {
    #[new]
    fn new() -> Self {
        Self::default()
    }

    fn wait_entered(&self, py: Python<'_>) -> PyResult<()> {
        let shared = Arc::clone(&self.shared);
        py.detach(move || shared.wait_for(|state| state.entered))
            .map_err(PyRuntimeError::new_err)
    }

    fn release(&self, py: Python<'_>) -> PyResult<()> {
        let shared = Arc::clone(&self.shared);
        // Separate gate mutex: this can never wait for the held engine mutex.
        py.detach(move || -> Result<(), &'static str> {
            let mut state = shared.state.lock().map_err(|_| "test gate poisoned")?;
            state.released = true;
            shared.changed.notify_all();
            Ok(())
        })
        .map_err(PyRuntimeError::new_err)
    }

    #[getter]
    fn timed_out(&self) -> bool {
        self.shared.timed_out.load(Ordering::SeqCst)
    }
}
