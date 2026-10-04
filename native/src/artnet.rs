//! Cached logical ArtDmx layout; wire padding never changes the logical stride.
use crate::{
    original::Original,
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

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum White {
    None,
    Zero,
    Brighter,
    Accurate,
}
pub(crate) struct Layout {
    packets: Vec<Vec<u8>>,
    spans: Vec<(usize, usize, usize, usize)>,
    short_spans: bool,
    order: [usize; 3],
    white: White,
    output_count: usize,
}
impl Layout {
    #[allow(clippy::too_many_arguments)]
    fn new(
        pixels: usize,
        universe: usize,
        size: usize,
        even: bool,
        start: usize,
        group: usize,
        pre: &[u8],
        post: &[u8],
        order: &str,
        white: &str,
    ) -> Result<Self, &'static str> {
        if pixels == 0
            || pixels > 32768 * 512
            || universe >= 32768
            || !(1..=512).contains(&size)
            || !(1..=512).contains(&start)
        {
            return Err("invalid Art-Net layout or exceeds universe 32767");
        }
        let order = match order {
            "RGB" => [0, 1, 2],
            "RBG" => [0, 2, 1],
            "GRB" => [1, 0, 2],
            "GBR" => [1, 2, 0],
            "BRG" => [2, 0, 1],
            "BGR" => [2, 1, 0],
            _ => return Err("invalid RGB order"),
        };
        let white = match white {
            "None" => White::None,
            "Zero" => White::Zero,
            "Brighter" => White::Brighter,
            "Accurate" => White::Accurate,
            _ => return Err("invalid white mode"),
        };
        let channels = if white == White::None { 3 } else { 4 };
        let group = if group == 0 || group > pixels {
            pixels
        } else {
            group
        };
        let groups = pixels / group;
        let group_channels = group * channels;
        let group_size = pre
            .len()
            .checked_add(group_channels)
            .and_then(|v| v.checked_add(post.len()))
            .ok_or("Art-Net layout overflow")?;
        let total = group_size
            .checked_mul(groups)
            .and_then(|v| v.checked_add(start - 1))
            .ok_or("Art-Net layout overflow")?;
        let universes = total.div_ceil(size);
        if universes > 32768 - universe {
            return Err("Art-Net channel layout exceeds universe 32767");
        }
        let wire = (size + usize::from(even && size % 2 == 1)).max(2);
        let mut packets = Vec::with_capacity(universes);
        for i in 0..universes {
            let mut p = vec![0; 18 + wire];
            p[..12].copy_from_slice(b"Art-Net\0\x00\x50\x00\x0e");
            p[14..16].copy_from_slice(&((universe + i) as u16).to_le_bytes());
            p[16..18].copy_from_slice(&(wire as u16).to_be_bytes());
            packets.push(p);
        }
        let mut spans = Vec::new();
        let mut logical = start - 1;
        for g in 0..groups {
            for &b in pre {
                packets[logical / size][18 + logical % size] = b;
                logical += 1;
            }
            let mut input = g * group_channels;
            let end = input + group_channels;
            while input < end {
                let n = (size - logical % size).min(end - input);
                spans.push((logical / size, 18 + logical % size, input, n));
                input += n;
                logical += n;
            }
            for &b in post {
                packets[logical / size][18 + logical % size] = b;
                logical += 1;
            }
        }
        Ok(Self {
            packets,
            spans,
            short_spans: cfg!(all(
                target_arch = "x86_64",
                any(
                    target_os = "linux",
                    target_os = "windows",
                    target_os = "macos"
                )
            )) && group_channels <= 32,
            order,
            white,
            output_count: pixels * channels,
        })
    }
    fn pack(&mut self, bytes: &[u8]) {
        if self.short_spans {
            for &(packet, slot, input, n) in &self.spans {
                short_copy(
                    &mut self.packets[packet][slot..slot + n],
                    &bytes[input..input + n],
                );
            }
        } else {
            self.pack_reference(bytes);
        }
    }
    fn pack_reference(&mut self, bytes: &[u8]) {
        for &(packet, slot, input, n) in &self.spans {
            self.packets[packet][slot..slot + n].copy_from_slice(&bytes[input..input + n]);
        }
    }
}

#[inline]
fn short_copy(output: &mut [u8], input: &[u8]) {
    let n = output.len();
    assert_eq!(n, input.len());
    match n {
        0 => {}
        1 => output[0] = input[0],
        2 => output.copy_from_slice(input),
        3 => {
            output[..2].copy_from_slice(&input[..2]);
            output[2] = input[2];
        }
        4..=7 => {
            output[..4].copy_from_slice(&input[..4]);
            output[n - 4..].copy_from_slice(&input[n - 4..]);
        }
        8..=15 => {
            output[..8].copy_from_slice(&input[..8]);
            output[n - 8..].copy_from_slice(&input[n - 8..]);
        }
        16..=32 => {
            output[..16].copy_from_slice(&input[..16]);
            output[n - 16..].copy_from_slice(&input[n - 16..]);
        }
        _ => output.copy_from_slice(input),
    }
}

fn numeric(
    input: &Original,
    layout: &Layout,
    bytes: &mut [u8],
    floats: &mut [f32],
    doubles: &mut [f64],
) -> Result<(), &'static str> {
    let width = if layout.white == White::None { 3 } else { 4 };
    let order = layout.order;
    macro_rules! integers {
        ($values:expr) => {{
            for (rgb, out) in $values.chunks_exact(3).zip(bytes.chunks_exact_mut(width)) {
                let w = rgb[0].min(rgb[1]).min(rgb[2]);
                // Only the final low byte is observed: subtraction modulo any original
                // integer width has the same low eight bits, including signed overflow.
                for j in 0..3 {
                    out[j] = if layout.white == White::Accurate {
                        (rgb[order[j]] as u8).wrapping_sub(w as u8)
                    } else {
                        rgb[order[j]] as u8
                    };
                }
                if width == 4 {
                    out[3] = if layout.white == White::Zero {
                        0
                    } else {
                        w as u8
                    };
                }
            }
        }};
    }
    macro_rules! floating {
        ($values:expr,$output:expr,$wrap:ident,$transform:ident) => {{
            if $values.iter().any(|v| !v.is_finite()) {
                return Err("nonfinite channel level");
            }
            crate::artnet_numeric::$transform($values, $output, order, layout.white);
            if crate::convert::$wrap($output, bytes) {
                return Err("nonfinite channel level after RGBW arithmetic");
            }
        }};
    }
    match input.kind {
        0 => crate::artnet_numeric::u8(&input.bytes, bytes, order, layout.white),
        1 => floating!(&input.floats, floats, wrap_f32, f32),
        2 => floating!(&input.doubles, doubles, wrap_f64, f64),
        3 => integers!(&input.signed),
        4 => integers!(&input.unsigned),
        _ => return Err("invalid Art-Net numeric kind"),
    }
    Ok(())
}
struct State<T: DatagramTransport> {
    layout: Layout,
    input: Original,
    bytes: Vec<u8>,
    floats: Vec<f32>,
    doubles: Vec<f64>,
    committed: Vec<Vec<u8>>,
    transport: T,
    destination: SocketAddrV4,
    sequence: u8,
    closed: bool,
    datagrams: u64,
    wire_bytes: u64,
    errors: u64,
    cleanup_error: Option<String>,
}
impl<T: DatagramTransport> State<T> {
    fn send(&mut self, converted: bool) -> io::Result<()> {
        if converted {
            self.bytes.copy_from_slice(&self.input.bytes);
        } else {
            numeric(
                &self.input,
                &self.layout,
                &mut self.bytes,
                &mut self.floats,
                &mut self.doubles,
            )
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        }
        self.layout.pack(&self.bytes);
        self.transmit()?;
        for (out, packet) in self.committed.iter_mut().zip(&self.layout.packets) {
            out.copy_from_slice(packet);
        }
        Ok(())
    }
    fn transmit(&mut self) -> io::Result<()> {
        for (i, p) in self.layout.packets.iter_mut().enumerate() {
            p[12] = self.sequence.wrapping_add(i as u8);
        }
        let deadline = Instant::now() + Duration::from_millis(200);
        for chunk in self.layout.packets.chunks(1024) {
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
                    self.sequence = self.sequence.wrapping_add(n as u8);
                    self.datagrams += n as u64;
                    self.wire_bytes += chunk[..n].iter().map(|p| p.len() as u64).sum::<u64>();
                    if n != chunk.len() {
                        self.errors += 1;
                        return Err(io::Error::new(io::ErrorKind::TimedOut, "partial UDP frame"));
                    }
                }
                Err(e) => {
                    self.errors += 1;
                    return Err(e);
                }
            }
        }
        Ok(())
    }
    fn close(&mut self, blackout: bool) {
        if self.closed {
            return;
        }
        if blackout {
            for p in &mut self.layout.packets {
                p[18..].fill(0);
            }
            if let Err(e) = self.transmit() {
                self.cleanup_error = Some(e.to_string());
            }
        }
        self.transport.close();
        self.closed = true;
    }
}
#[pyclass]
pub struct ArtNetEngine {
    inner: Mutex<State<Transport>>,
    count: usize,
    output_count: usize,
}
#[pymethods]
impl ArtNetEngine {
    #[new]
    #[pyo3(signature=(pixels,destination,universe,packet_size,even_packet_size,dmx_start_address,pixels_per_device,pre_amble,post_amble,rgb_order,white_mode,broadcast,mode="socket",backend="batched",batch_size=64,override_destination=None))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        pixels: usize,
        destination: &str,
        universe: usize,
        packet_size: usize,
        even_packet_size: bool,
        dmx_start_address: usize,
        pixels_per_device: usize,
        pre_amble: Vec<u8>,
        post_amble: Vec<u8>,
        rgb_order: &str,
        white_mode: &str,
        broadcast: bool,
        mode: &str,
        backend: &str,
        batch_size: usize,
        override_destination: Option<&str>,
    ) -> PyResult<Self> {
        let layout = Layout::new(
            pixels,
            universe,
            packet_size,
            even_packet_size,
            dmx_start_address,
            pixels_per_device,
            &pre_amble,
            &post_amble,
            rgb_order,
            white_mode,
        )
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
            "socket" => {
                let t = SocketTransport::new(backend == "batched", batch_size)
                    .map_err(PyOSError::new_err)?;
                t.set_broadcast(broadcast).map_err(PyOSError::new_err)?;
                Transport::Socket(t)
            }
            "capture" | "discard" => Transport::Memory(MemoryTransport {
                capture: mode == "capture",
                packets: Vec::new(),
            }),
            _ => return Err(PyValueError::new_err("invalid transport mode")),
        };
        let n = layout.output_count;
        Ok(Self {
            count: pixels * 3,
            output_count: n,
            inner: Mutex::new(State {
                committed: layout.packets.clone(),
                layout,
                input: Original::default(),
                bytes: vec![0; n],
                floats: vec![0.0; n],
                doubles: vec![0.0; n],
                transport,
                destination,
                sequence: 0,
                closed: false,
                datagrams: 0,
                wire_bytes: 0,
                errors: 0,
                cleanup_error: None,
            }),
        })
    }
    fn send(&self, py: Python<'_>, frame: &Bound<'_, PyAny>, kind: u8) -> PyResult<()> {
        // Kind 6 is checked post-arithmetic u8 normalization for rare float formats.
        if kind > 4 && kind != 6 {
            return Err(PyValueError::new_err("invalid Art-Net numeric kind"));
        }
        let view = PyMemoryView::from(frame)?;
        let result = {
            let mut s = self
                .inner
                .lock_py_attached(py)
                .map_err(|_| PyOSError::new_err("engine lock poisoned"))?;
            if s.closed {
                return Err(PyRuntimeError::new_err("sender closed"));
            }
            s.input.snapshot(
                py,
                &view,
                if kind == 6 { 0 } else { kind },
                if kind == 6 {
                    self.output_count
                } else {
                    self.count
                },
            )?;
            let s = &mut *s;
            py.detach(|| s.send(kind == 6))
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
    #[pyo3(signature=(blackout=true))]
    fn close(&self, py: Python<'_>, blackout: bool) -> PyResult<()> {
        let mut s = self
            .inner
            .lock_py_attached(py)
            .map_err(|_| PyOSError::new_err("engine lock poisoned"))?;
        let s = &mut *s;
        py.detach(|| s.close(blackout));
        Ok(())
    }
    fn cleanup_error(&self, py: Python<'_>) -> PyResult<Option<String>> {
        Ok(self
            .inner
            .lock_py_attached(py)
            .map_err(|_| PyOSError::new_err("engine lock poisoned"))?
            .cleanup_error
            .clone())
    }
    fn _test_broadcast(&self, py: Python<'_>) -> PyResult<bool> {
        let s = self
            .inner
            .lock_py_attached(py)
            .map_err(|_| PyOSError::new_err("engine lock poisoned"))?;
        match &s.transport {
            Transport::Socket(t) => t.broadcast().map_err(PyOSError::new_err),
            _ => Err(PyValueError::new_err("requires socket transport")),
        }
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
#[path = "../tests/artnet.rs"]
mod tests;
