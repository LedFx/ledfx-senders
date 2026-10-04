//! Exact owned numeric snapshots; dyadics are used only for mixed/rare formats.
use pyo3::{buffer::PyBuffer, exceptions::PyValueError, prelude::*, types::PyMemoryView};

#[derive(Default)]
pub struct Original {
    pub kind: u8,
    pub bytes: Vec<u8>,
    pub floats: Vec<f32>,
    pub doubles: Vec<f64>,
    pub signed: Vec<i64>,
    pub unsigned: Vec<u64>,
    pub rare: Vec<u64>,
}
#[derive(PartialEq, Eq)]
struct Dyadic {
    negative: bool,
    magnitude: u128,
    exponent: i32,
}
impl Dyadic {
    fn new(negative: bool, magnitude: u128, exponent: i32) -> Self {
        let shift = if magnitude == 0 {
            0
        } else {
            magnitude.trailing_zeros()
        };
        Self {
            negative: negative && magnitude != 0,
            magnitude: magnitude >> shift,
            exponent: if magnitude == 0 {
                0
            } else {
                exponent + shift as i32
            },
        }
    }
    fn float(value: f64) -> Self {
        let bits = value.to_bits();
        let exponent = ((bits >> 52) & 2047) as i32;
        let fraction = bits & ((1u64 << 52) - 1);
        Self::new(
            bits >> 63 != 0,
            (fraction | if exponent == 0 { 0 } else { 1u64 << 52 }) as u128,
            if exponent == 0 {
                -1074
            } else {
                exponent - 1075
            },
        )
    }
    fn truncated(&self) -> Option<u128> {
        if self.exponent < 0 {
            Some(
                self.magnitude
                    .checked_shr((-self.exponent) as u32)
                    .unwrap_or(0),
            )
        } else if self.magnitude == 0 {
            Some(0)
        } else if self.exponent >= 128 || self.magnitude.leading_zeros() < self.exponent as u32 {
            None
        } else {
            Some(self.magnitude << self.exponent)
        }
    }
    fn osc(&self) -> Result<i64, &'static str> {
        let n = self
            .truncated()
            .ok_or("OSC level outside signed 64-bit range")?;
        // Negative fractional values below MIN are outside the original-value domain.
        let limit = if self.negative {
            1u128 << 63
        } else {
            (1u128 << 63) - 1
        };
        if n > limit || (self.negative && n == limit && self != &Self::new(true, limit, 0)) {
            return Err("OSC level outside signed 64-bit range");
        }
        Ok(if self.negative {
            (n as i128).wrapping_neg() as i64
        } else {
            n as i64
        })
    }
    fn wrap(&self) -> u8 {
        let Some(n) = self.truncated() else { return 0 };
        if n >= 1 << 31 && !(self.negative && self == &Self::new(true, 1 << 31, 0)) {
            return 0;
        }
        if self.negative {
            0u8.wrapping_sub(n as u8)
        } else {
            n as u8
        }
    }
}
impl Original {
    pub fn snapshot(
        &mut self,
        py: Python<'_>,
        view: &Bound<'_, PyMemoryView>,
        kind: u8,
        count: usize,
    ) -> PyResult<()> {
        fn copy<T: pyo3::buffer::Element + Copy + Default>(
            py: Python<'_>,
            view: &Bound<'_, PyMemoryView>,
            output: &mut Vec<T>,
            count: usize,
        ) -> PyResult<()> {
            let buffer = PyBuffer::<T>::get(view.as_any())?;
            if !buffer.is_c_contiguous() {
                return Err(PyValueError::new_err("noncontiguous input"));
            }
            output.resize(count, T::default());
            buffer.copy_to_slice(py, output)
        }
        match kind {
            0 => copy(py, view, &mut self.bytes, count)?,
            1 => copy(py, view, &mut self.floats, count)?,
            2 => copy(py, view, &mut self.doubles, count)?,
            3 => copy(py, view, &mut self.signed, count)?,
            4 => copy(py, view, &mut self.unsigned, count)?,
            5 => copy(py, view, &mut self.rare, count * 3)?,
            _ => return Err(PyValueError::new_err("invalid original numeric kind")),
        }
        self.kind = kind;
        Ok(())
    }
    fn dyadic(&self, i: usize) -> Dyadic {
        match self.kind {
            0 => Dyadic::new(false, self.bytes[i] as u128, 0),
            1 => Dyadic::float(self.floats[i] as f64),
            2 => Dyadic::float(self.doubles[i]),
            3 => Dyadic::new(self.signed[i] < 0, self.signed[i].unsigned_abs() as u128, 0),
            4 => Dyadic::new(false, self.unsigned[i] as u128, 0),
            _ => Dyadic::new(
                self.rare[i * 3 + 2] >> 63 != 0,
                ((self.rare[i * 3] as u128) << 64) | self.rare[i * 3 + 1] as u128,
                self.rare[i * 3 + 2] as u32 as i32,
            ),
        }
    }
    pub fn changed(&self, previous: &Self, initialized: bool, mask: &mut [bool]) -> usize {
        if !initialized {
            mask.fill(true);
            return mask.len();
        }
        macro_rules! compare {
            ($a:expr,$b:expr) => {
                for ((a, b), changed) in $a
                    .chunks_exact(3)
                    .zip($b.chunks_exact(3))
                    .zip(mask.iter_mut())
                {
                    *changed = a != b;
                }
            };
        }
        if self.kind == previous.kind {
            match self.kind {
                0 => compare!(self.bytes, previous.bytes),
                1 => compare!(self.floats, previous.floats),
                2 => return crate::change_mask::compare(&self.doubles, &previous.doubles, mask),
                3 => compare!(self.signed, previous.signed),
                4 => compare!(self.unsigned, previous.unsigned),
                _ => {
                    for (i, changed) in mask.iter_mut().enumerate() {
                        *changed = (i * 3..i * 3 + 3).any(|j| self.dyadic(j) != previous.dyadic(j));
                    }
                }
            }
        } else {
            for (i, changed) in mask.iter_mut().enumerate() {
                *changed = (i * 3..i * 3 + 3).any(|j| self.dyadic(j) != previous.dyadic(j));
            }
        }
        mask.iter().filter(|&&v| v).count()
    }
    /// Validate and convert into scratch. No persistent wire/state mutation.
    pub fn encode(
        &self,
        osc: bool,
        bytes: &mut [u8],
        floats: &mut [f32],
    ) -> Result<(), &'static str> {
        let count = bytes.len();
        if osc && self.kind == 2 {
            return if crate::osc_numeric::convert(&self.doubles, floats) {
                Err("OSC level outside finite signed 64-bit range")
            } else {
                Ok(())
            };
        }

        if self.kind == 5
            && self.rare.chunks_exact(3).any(|token| {
                token[2] & 0x7fffffff00000000 != 0
                    || !(-32768..=32768).contains(&(token[2] as u32 as i32))
            })
        {
            return Err("invalid extended numeric token");
        }

        if self.kind == 1 && self.floats.iter().any(|x| !x.is_finite())
            || self.kind == 2 && self.doubles.iter().any(|x| !x.is_finite())
        {
            return Err("nonfinite channel level");
        }
        if !osc {
            match self.kind {
                0 => bytes.copy_from_slice(&self.bytes),
                1 => {
                    crate::convert::wrap_f32(&self.floats, bytes);
                }
                2 => {
                    crate::convert::wrap_f64(&self.doubles, bytes);
                }
                3 => {
                    for (out, &v) in bytes.iter_mut().zip(&self.signed) {
                        *out = v as u8
                    }
                }
                4 => {
                    for (out, &v) in bytes.iter_mut().zip(&self.unsigned) {
                        *out = v as u8
                    }
                }
                _ => {
                    for (i, out) in bytes.iter_mut().enumerate() {
                        *out = self.dyadic(i).wrap()
                    }
                }
            }
        } else {
            for (i, out) in floats.iter_mut().enumerate().take(count) {
                let n = match self.kind {
                    0 => self.bytes[i] as i64,
                    1 | 2 => {
                        let v = if self.kind == 1 {
                            self.floats[i] as f64
                        } else {
                            self.doubles[i]
                        };
                        if !(-9223372036854775808.0..9223372036854775808.0).contains(&v) {
                            return Err("OSC level outside signed 64-bit range");
                        }
                        v as i64
                    }
                    3 => self.signed[i],
                    4 => i64::try_from(self.unsigned[i])
                        .map_err(|_| "OSC level outside signed 64-bit range")?,
                    _ => self.dyadic(i).osc()?,
                };
                *out = ((n as f64) / 255.0) as f32;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        hint::black_box,
        time::{Duration, Instant},
    };
    fn compiler_osc(input: &[f64], output: &mut [f32]) -> bool {
        let invalid = input.iter().fold(false, |bad, &v| {
            bad | !(-9223372036854775808.0..9223372036854775808.0).contains(&v)
        });
        for (out, &v) in output.iter_mut().zip(input) {
            let n = v.trunc();
            *out = (if n == 0.0 { 0.0 } else { n / 255.0 }) as f32;
        }
        invalid
    }
    #[test]
    fn compiler_conversion_matches_i64_oracle_and_signed_zero() {
        for count in 1..66 {
            let input: Vec<_> = (0..count)
                .map(|i| {
                    if i % 2 == 0 {
                        i as f64 * 12345678.123
                    } else {
                        -(i as f64) * 0.75
                    }
                })
                .collect();
            let mut output = vec![0.0; count];
            assert!(!compiler_osc(&input, &mut output));
            for (&v, out) in input.iter().zip(output) {
                assert_eq!(
                    out.to_bits(),
                    (((v as i64) as f64 / 255.0) as f32).to_bits()
                )
            }
        }
        let input = [
            -0.9,
            -0.0,
            0.0,
            -9223372036854775808.0,
            f64::from_bits(9223372036854775808.0f64.to_bits() - 1),
        ];
        let mut output = [0.0; 5];
        assert!(!compiler_osc(&input, &mut output));
        for (v, out) in input.into_iter().zip(output) {
            assert_eq!(
                out.to_bits(),
                (((v as i64) as f64 / 255.0) as f32).to_bits()
            )
        }
        for bad in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            9223372036854775808.0,
        ] {
            assert!(compiler_osc(&[bad], &mut [0.0]))
        }
    }
    #[test]
    #[ignore = "manual isolated kernel measurement"]
    fn benchmark_stateful_kernels() {
        let count = 150000;
        let input: Vec<_> = (0..count).map(|i| (i % 256) as f64 + 0.75).collect();
        let mut output = vec![0.0; count];
        let mut bytes = vec![0; count];
        let source = Original {
            kind: 2,
            doubles: input.clone(),
            ..Original::default()
        };
        let previous = Original {
            kind: 2,
            doubles: input.clone(),
            ..Original::default()
        };
        let mut mask = vec![false; count / 3];
        for repeat in 0..3 {
            for kernel in [
                "selected-osc",
                "compiler-osc",
                "original-mask",
                "realtime-wrap",
            ] {
                let start = Instant::now();
                let mut frames = 0;
                while start.elapsed() < Duration::from_secs(2) {
                    match kernel {
                        "selected-osc" => {
                            source
                                .encode(true, black_box(&mut bytes), black_box(&mut output))
                                .unwrap();
                        }
                        "compiler-osc" => {
                            black_box(compiler_osc(black_box(&input), black_box(&mut output)));
                        }
                        "original-mask" => {
                            black_box(source.changed(
                                black_box(&previous),
                                true,
                                black_box(&mut mask),
                            ));
                        }
                        _ => {
                            source
                                .encode(false, black_box(&mut bytes), black_box(&mut output))
                                .unwrap();
                        }
                    }
                    frames += 1;
                }
                println!(
                    "{{\"kernel\":\"{kernel}\",\"repeat\":{repeat},\"channels\":{count},\"frames\":{frames},\"seconds\":{}}}",
                    start.elapsed().as_secs_f64()
                );
            }
        }
    }
}
