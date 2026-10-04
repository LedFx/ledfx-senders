//! Vendor byte encoding only. The caller retains its serial/SDK/DTLS transport.
use crate::{buffer::NumericPolicy, original::Original};
use pyo3::{
    exceptions::{PyOverflowError, PyValueError},
    prelude::*,
    sync::MutexExt,
    types::{PyBytes, PyMemoryView},
};
use std::sync::Mutex;

#[derive(Default)]
pub(crate) struct Channels {
    original: Original,
    converted: Vec<u8>,
}
impl Channels {
    pub(crate) fn snapshot(
        &mut self,
        py: Python<'_>,
        view: &Bound<'_, PyMemoryView>,
        kind: u8,
        count: usize,
    ) -> PyResult<()> {
        if kind > 2 {
            return Err(PyValueError::new_err("unsupported normalized format"));
        }
        self.original.snapshot(py, view, kind, count)
    }
    pub(crate) fn convert(&mut self, policy: NumericPolicy) -> Result<&[u8], &'static str> {
        let input = &self.original;
        if input.kind == 0 {
            return Ok(&input.bytes);
        }
        let count = if input.kind == 1 {
            input.floats.len()
        } else {
            input.doubles.len()
        };
        self.converted.resize(count, 0);
        let invalid = match (input.kind, policy) {
            (1, NumericPolicy::Wrap) => {
                crate::convert::wrap_f32(&input.floats, &mut self.converted)
            }
            (1, NumericPolicy::Clip) => {
                crate::convert::clip_f32(&input.floats, &mut self.converted)
            }
            (1, NumericPolicy::Strict) => {
                crate::convert::strict_f32(&input.floats, &mut self.converted)
            }
            (_, NumericPolicy::Wrap) => {
                crate::convert::wrap_f64(&input.doubles, &mut self.converted)
            }
            (_, NumericPolicy::Clip) => {
                crate::convert::clip_f64(&input.doubles, &mut self.converted)
            }
            (_, NumericPolicy::Strict) => {
                crate::convert::strict_f64(&input.doubles, &mut self.converted)
            }
        };
        if invalid {
            if matches!(policy, NumericPolicy::Strict) {
                let first = if input.kind == 1 {
                    input
                        .floats
                        .iter()
                        .find(|x| !x.is_finite())
                        .map(|x| *x as f64)
                } else {
                    input.doubles.iter().find(|x| !x.is_finite()).copied()
                };
                if first.is_some_and(|x| x.is_infinite()) {
                    return Err("infinite channel level");
                }
            }
            return Err("invalid channel level");
        }
        Ok(&self.converted)
    }
}

pub(crate) fn error(message: &'static str) -> PyErr {
    if message == "infinite channel level" {
        PyOverflowError::new_err(message)
    } else {
        PyValueError::new_err(message)
    }
}

#[pyclass]
pub struct RGBGather {
    indices: Vec<usize>,
    inner: Mutex<Channels>,
}
#[pymethods]
impl RGBGather {
    #[new]
    fn new(indices: Vec<usize>) -> PyResult<Self> {
        if indices.is_empty() || indices.len() > 1_000_000 {
            return Err(PyValueError::new_err("invalid RGB count"));
        }
        let mut seen = vec![false; indices.len()];
        for &index in &indices {
            if index >= seen.len() || seen[index] {
                return Err(PyValueError::new_err(
                    "indices must form a complete permutation",
                ));
            }
            seen[index] = true;
        }
        Ok(Self {
            indices,
            inner: Mutex::new(Channels::default()),
        })
    }
    fn encode(&self, py: Python<'_>, frame: &Bound<'_, PyAny>, kind: u8) -> PyResult<Py<PyBytes>> {
        let view = PyMemoryView::from(frame)?;
        let result = {
            let mut state = self
                .inner
                .lock_py_attached(py)
                .map_err(|_| PyValueError::new_err("encoder lock poisoned"))?;
            state.snapshot(py, &view, kind, self.indices.len() * 3)?;
            let state = &mut *state;
            py.detach(|| {
                let rgb = state.convert(NumericPolicy::Wrap)?;
                let mut output = Vec::with_capacity(rgb.len());
                for &index in &self.indices {
                    output.extend_from_slice(&rgb[index * 3..index * 3 + 3]);
                }
                Ok::<_, &'static str>(output)
            })
        };
        drop(view);
        Ok(PyBytes::new(py, &result.map_err(error)?).unbind())
    }
}

fn encode<F>(
    py: Python<'_>,
    frame: &Bound<'_, PyAny>,
    kind: u8,
    count: usize,
    policy: NumericPolicy,
    pack: F,
) -> PyResult<Py<PyBytes>>
where
    F: FnOnce(&[u8]) -> Vec<u8> + Send,
{
    let view = PyMemoryView::from(frame)?;
    let mut channels = Channels::default();
    channels.snapshot(py, &view, kind, count * 3)?;
    let result = py.detach(|| channels.convert(policy).map(pack));
    drop(view);
    Ok(PyBytes::new(py, &result.map_err(error)?).unbind())
}

#[pyfunction]
pub fn encode_adalight(
    py: Python<'_>,
    frame: &Bound<'_, PyAny>,
    kind: u8,
    count: usize,
    order: &str,
) -> PyResult<Py<PyBytes>> {
    if !(1..=65536).contains(&count) {
        return Err(PyValueError::new_err("invalid Adalight count"));
    }
    let indices = match order {
        "RGB" => [0, 1, 2],
        "RBG" => [0, 2, 1],
        "GRB" => [1, 0, 2],
        "GBR" => [1, 2, 0],
        "BRG" => [2, 0, 1],
        "BGR" => [2, 1, 0],
        _ => return Err(PyValueError::new_err("invalid RGB order")),
    };
    encode(py, frame, kind, count, NumericPolicy::Wrap, |rgb| {
        pack_adalight(rgb, count, indices)
    })
}

#[pyfunction]
pub fn encode_openrgb(
    py: Python<'_>,
    frame: &Bound<'_, PyAny>,
    kind: u8,
    count: usize,
    device_id: u32,
) -> PyResult<Py<PyBytes>> {
    if !(1..=65535).contains(&count) {
        return Err(PyValueError::new_err("invalid OpenRGB count"));
    }
    encode(py, frame, kind, count, NumericPolicy::Wrap, |rgb| {
        pack_openrgb(rgb, count, device_id)
    })
}

#[pyfunction]
#[allow(clippy::too_many_arguments)]
pub fn encode_hue(
    py: Python<'_>,
    frame: &Bound<'_, PyAny>,
    kind: u8,
    count: usize,
    identifier: Vec<u8>,
    ids: Vec<u8>,
    sequence: u8,
) -> PyResult<Py<PyBytes>> {
    if !(1..=256).contains(&count)
        || identifier.len() != 36
        || ids.len() != count
        || ids.iter().enumerate().any(|(i, id)| ids[..i].contains(id))
    {
        return Err(PyValueError::new_err("invalid Hue metadata"));
    }
    encode(py, frame, kind, count, NumericPolicy::Strict, |rgb| {
        pack_hue(rgb, count, &identifier, &ids, sequence)
    })
}

#[pyfunction]
pub fn encode_govee(
    py: Python<'_>,
    frame: &Bound<'_, PyAny>,
    kind: u8,
    count: usize,
    stretch: bool,
) -> PyResult<Py<PyBytes>> {
    if !(1..=255).contains(&count) {
        return Err(PyValueError::new_err("invalid Govee count"));
    }
    encode(py, frame, kind, count, NumericPolicy::Wrap, |rgb| {
        pack_govee(rgb, count, stretch)
    })
}

// Portable compiler loops: conservative provisional cutoffs under paired review.
const MEASURED_PACKING: bool = cfg!(any(
    all(
        target_arch = "x86_64",
        any(
            target_os = "linux",
            target_os = "windows",
            target_os = "macos"
        )
    ),
    all(
        target_arch = "aarch64",
        any(target_os = "linux", target_os = "macos")
    )
));
fn pack_adalight(rgb: &[u8], count: usize, indices: [usize; 3]) -> Vec<u8> {
    if indices == [0, 1, 2] {
        return pack_adalight_identity(rgb, count);
    }
    if MEASURED_PACKING && count >= 128 {
        pack_adalight_bulk(rgb, count, indices)
    } else {
        pack_adalight_reference(rgb, count, indices)
    }
}
fn pack_openrgb(rgb: &[u8], count: usize, device_id: u32) -> Vec<u8> {
    if MEASURED_PACKING && !cfg!(all(target_arch = "x86_64", target_os = "macos")) && count >= 128 {
        pack_openrgb_bulk(rgb, count, device_id)
    } else {
        pack_openrgb_reference(rgb, count, device_id)
    }
}

fn pack_adalight_identity(rgb: &[u8], count: usize) -> Vec<u8> {
    let [high, low] = ((count - 1) as u16).to_be_bytes();
    let mut output = Vec::with_capacity(6 + rgb.len());
    output.extend_from_slice(&[b'A', b'd', b'a', high, low, high ^ low ^ 0x55]);
    output.extend_from_slice(rgb);
    output
}

fn pack_adalight_reference(rgb: &[u8], count: usize, indices: [usize; 3]) -> Vec<u8> {
    let n = (count - 1) as u16;
    let [hi, lo] = n.to_be_bytes();
    let mut out = Vec::with_capacity(6 + rgb.len());
    out.extend_from_slice(&[b'A', b'd', b'a', hi, lo, hi ^ lo ^ 0x55]);
    for pixel in rgb.chunks_exact(3) {
        out.extend_from_slice(&[pixel[indices[0]], pixel[indices[1]], pixel[indices[2]]]);
    }
    out
}

fn pack_openrgb_reference(rgb: &[u8], count: usize, device_id: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(22 + count * 4);
    out.extend_from_slice(b"ORGB");
    out.extend_from_slice(&device_id.to_le_bytes());
    out.extend_from_slice(&1050u32.to_le_bytes());
    let size = (6 + count * 4) as u32;
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&(count as u16).to_le_bytes());
    for pixel in rgb.chunks_exact(3) {
        out.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 0]);
    }
    out
}

fn pack_hue(rgb: &[u8], count: usize, identifier: &[u8], ids: &[u8], sequence: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(52 + count * 7);
    out.extend_from_slice(b"HueStream");
    out.extend_from_slice(&[2, 0, sequence, 0, 0, 0, 0]);
    out.extend_from_slice(identifier);
    for (pixel, id) in rgb.chunks_exact(3).zip(ids.iter().copied()) {
        out.extend_from_slice(&[
            id, pixel[0], pixel[0], pixel[1], pixel[1], pixel[2], pixel[2],
        ]);
    }
    out
}

fn pack_govee(rgb: &[u8], count: usize, stretch: bool) -> Vec<u8> {
    let mut raw = Vec::with_capacity(7 + rgb.len());
    raw.extend_from_slice(&[0xbb, 0, 0xfa, 0xb0, u8::from(stretch), count as u8]);
    raw.extend_from_slice(rgb);
    raw.push(raw.iter().fold(0, |sum, b| sum ^ b));
    let mut out = Vec::with_capacity(64 + raw.len().div_ceil(3) * 4);
    out.extend_from_slice(b"{\"msg\": {\"cmd\": \"razer\", \"data\": {\"pt\": \"");
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for chunk in raw.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        out.extend_from_slice(&[
            B64[(a >> 2) as usize],
            B64[(((a & 3) << 4) | (b >> 4)) as usize],
            if chunk.len() > 1 {
                B64[(((b & 15) << 2) | (c >> 6)) as usize]
            } else {
                b'='
            },
            if chunk.len() > 2 {
                B64[(c & 63) as usize]
            } else {
                b'='
            },
        ]);
    }
    out.extend_from_slice(b"\"}}}");
    out
}

fn pack_adalight_bulk(rgb: &[u8], count: usize, indices: [usize; 3]) -> Vec<u8> {
    let mut output = vec![0; 6 + rgb.len()];
    let [high, low] = ((count - 1) as u16).to_be_bytes();
    output[..6].copy_from_slice(&[b'A', b'd', b'a', high, low, high ^ low ^ 0x55]);
    for (out, pixel) in output[6..].chunks_exact_mut(3).zip(rgb.chunks_exact(3)) {
        out[0] = pixel[indices[0]];
        out[1] = pixel[indices[1]];
        out[2] = pixel[indices[2]];
    }
    output
}
fn pack_openrgb_bulk(rgb: &[u8], count: usize, device_id: u32) -> Vec<u8> {
    let mut output = vec![0; 22 + count * 4];
    output[..4].copy_from_slice(b"ORGB");
    output[4..8].copy_from_slice(&device_id.to_le_bytes());
    output[8..12].copy_from_slice(&1050u32.to_le_bytes());
    output[12..16].copy_from_slice(&((count * 4 + 6) as u32).to_le_bytes());
    output[16..20].copy_from_slice(&((count * 4 + 6) as u32).to_le_bytes());
    output[20..22].copy_from_slice(&(count as u16).to_le_bytes());
    for (out, pixel) in output[22..].chunks_exact_mut(4).zip(rgb.chunks_exact(3)) {
        out[0] = pixel[0];
        out[1] = pixel[1];
        out[2] = pixel[2];
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{hint::black_box, time::Instant};

    #[test]
    #[ignore = "bounded paired compiler packing candidate"]
    fn benchmark_bulk_packing() {
        for pixels in [1, 30, 127, 128, 129, 170, 1024, 50000] {
            let input: Vec<u8> = (0..pixels * 3).map(|i| i as u8).collect();
            let loops = if pixels < 1024 { 10000 } else { 1000 };
            for trial in 0..7 {
                for protocol in ["adalight", "openrgb"] {
                    for candidate in if trial % 2 == 0 {
                        [false, true]
                    } else {
                        [true, false]
                    } {
                        let begin = Instant::now();
                        for _ in 0..loops {
                            let output = match (protocol, candidate) {
                                ("adalight", false) => pack_adalight_reference(
                                    black_box(&input),
                                    pixels,
                                    black_box([2, 0, 1]),
                                ),
                                ("adalight", true) => pack_adalight_bulk(
                                    black_box(&input),
                                    pixels,
                                    black_box([2, 0, 1]),
                                ),
                                (_, false) => pack_openrgb_reference(black_box(&input), pixels, 0),
                                (_, true) => pack_openrgb_bulk(black_box(&input), pixels, 0),
                            };
                            black_box(output);
                        }
                        println!(
                            "bulk-pack pixels={pixels} protocol={protocol} trial={trial} candidate={candidate} ns={}",
                            begin.elapsed().as_nanos() / loops
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn identity_adalight_copy_preserves_headers_offsets_and_tails() {
        for (pixels, header) in [
            (1, [65, 100, 97, 0, 0, 85]),
            (17, [65, 100, 97, 0, 16, 69]),
            (129, [65, 100, 97, 0, 128, 213]),
            (65536, [65, 100, 97, 255, 255, 85]),
        ] {
            for offset in 0..32 {
                let source: Vec<u8> = (0..pixels * 3 + offset).map(|i| (i * 79) as u8).collect();
                let rgb = &source[offset..];
                let encoded = pack_adalight_identity(rgb, pixels);
                assert_eq!(&encoded[..6], &header);
                assert_eq!(&encoded[6..], rgb);
                assert_eq!(encoded, pack_adalight_reference(rgb, pixels, [0, 1, 2]));
            }
        }
    }

    #[test]
    fn bulk_packing_matches_all_orders_offsets_and_tails() {
        for pixels in [
            1, 2, 3, 4, 5, 7, 8, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129, 170, 171, 1024,
        ] {
            for offset in 0..32 {
                let source: Vec<u8> = (0..pixels * 3 + offset).map(|i| (i * 79) as u8).collect();
                let rgb = &source[offset..];
                for order in [
                    [0, 1, 2],
                    [0, 2, 1],
                    [1, 0, 2],
                    [1, 2, 0],
                    [2, 0, 1],
                    [2, 1, 0],
                ] {
                    assert_eq!(
                        pack_adalight(rgb, pixels, order),
                        pack_adalight_reference(rgb, pixels, order)
                    );
                    assert_eq!(
                        pack_adalight_bulk(rgb, pixels, order),
                        pack_adalight_reference(rgb, pixels, order)
                    );
                }
                assert_eq!(
                    pack_openrgb(rgb, pixels, 0x12345678),
                    pack_openrgb_reference(rgb, pixels, 0x12345678)
                );
                assert_eq!(
                    pack_openrgb_bulk(rgb, pixels, 0x12345678),
                    pack_openrgb_reference(rgb, pixels, 0x12345678)
                );
            }
        }
    }

    #[test]
    #[ignore = "bounded stage timing, run separately from correctness tests"]
    fn benchmark_vendor_stages() {
        for pixels in [10, 30, 170, 255, 1024, 50000] {
            let input: Vec<u8> = (0..pixels * 3).map(|i| i as u8).collect();
            let floats: Vec<f64> = input.iter().map(|&x| x as f64 + 0.25).collect();
            let mut converted = vec![0; input.len()];
            let repeats = if pixels < 1024 { 10000 } else { 1000 };
            for trial in 0..7 {
                for operation in if trial % 2 == 0 {
                    [0, 1, 2, 3, 4]
                } else {
                    [4, 3, 2, 1, 0]
                } {
                    if (operation == 3 && pixels > 255) || (operation == 4 && pixels > 256) {
                        continue;
                    }
                    let ids: Vec<u8> = (0..pixels.min(256)).map(|i| i as u8).collect();
                    let started = Instant::now();
                    for _ in 0..repeats {
                        match operation {
                            0 => {
                                black_box(crate::convert::wrap_f64(
                                    black_box(&floats),
                                    black_box(&mut converted),
                                ));
                            }
                            1 => {
                                black_box(pack_adalight(black_box(&input), pixels, [2, 0, 1]));
                            }
                            2 => {
                                black_box(pack_openrgb(black_box(&input), pixels, 0));
                            }
                            3 => {
                                black_box(pack_govee(black_box(&input), pixels, false));
                            }
                            _ => {
                                black_box(pack_hue(
                                    black_box(&input),
                                    pixels,
                                    b"12345678-1234-1234-1234-123456789abc",
                                    &ids,
                                    0,
                                ));
                            }
                        }
                    }
                    println!(
                        "vendor-stage pixels={pixels} operation={operation} trial={trial} ns={}",
                        started.elapsed().as_nanos() / repeats
                    );
                }
            }
        }
    }
}
