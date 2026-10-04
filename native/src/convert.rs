//! Checked channel conversion, specialized at compile time by numeric policy.
//! 0 = DDP wrapping, 1 = E1.31 strict (-1, 256), 2 = OPC clamp/truncate.
//! Scratch output is never published until the entire frame is valid.

#[inline]
fn scalar(value: f64) -> u8 {
    // NumPy's normal finite float-to-uint8 path truncates through signed i32.
    // Preserve the measured original portable range-check path.
    if (-2147483648.0..2147483648.0).contains(&value) {
        value as i32 as u8
    } else {
        0
    }
}

fn dispatch_f32<const POLICY: u8>(input: &[f32], output: &mut [u8]) -> bool {
    assert_eq!(input.len(), output.len());
    #[cfg(target_arch = "x86_64")]
    if x86::avx512_available() {
        // SAFETY: all required CPU/OS features checked; lengths match.
        return unsafe { x86::f32_avx512::<POLICY>(input, output) };
    }
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: feature detection dominates this call; slice lengths match.
        return unsafe { x86::f32_avx2::<POLICY>(input, output) };
    }
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("sse2") {
        // SAFETY: checked feature and equal slice lengths.
        return unsafe { x86::f32_sse2::<POLICY>(input, output) };
    }
    // On native Linux ARM and Apple Silicon, LLVM's wider vectorized loop
    // beats the explicit four-lane kernel for Strict/Clip (see hosted receipts).
    #[cfg(target_arch = "aarch64")]
    if POLICY != 0 {
        return scalar_f32::<POLICY>(input, output);
    }
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("neon") {
        // SAFETY: checked feature and equal slice lengths.
        return unsafe { arm::f32_neon::<POLICY>(input, output) };
    }
    scalar_f32::<POLICY>(input, output)
}
fn dispatch_f64<const POLICY: u8>(input: &[f64], output: &mut [u8]) -> bool {
    assert_eq!(input.len(), output.len());
    #[cfg(target_arch = "x86_64")]
    if x86::avx512_available() {
        // SAFETY: all required CPU/OS features checked; lengths match.
        return unsafe { x86::f64_avx512::<POLICY>(input, output) };
    }
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: feature detection dominates this call; slice lengths match.
        return unsafe { x86::f64_avx2::<POLICY>(input, output) };
    }
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("sse2") {
        // SAFETY: checked feature and equal slice lengths.
        return unsafe { x86::f64_sse2::<POLICY>(input, output) };
    }
    // Clip's compiler loop uses SIMD validation plus scalar conversion and
    // wins native ARM trials against the explicit packed f64 kernel.
    #[cfg(target_arch = "aarch64")]
    if POLICY == 2 {
        return scalar_f64::<POLICY>(input, output);
    }
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("neon") {
        // SAFETY: checked feature and equal slice lengths.
        return unsafe { arm::f64_neon::<POLICY>(input, output) };
    }
    scalar_f64::<POLICY>(input, output)
}
pub fn wrap_f32(input: &[f32], output: &mut [u8]) -> bool {
    dispatch_f32::<0>(input, output)
}
pub fn wrap_f64(input: &[f64], output: &mut [u8]) -> bool {
    dispatch_f64::<0>(input, output)
}
pub fn strict_f32(input: &[f32], output: &mut [u8]) -> bool {
    dispatch_f32::<1>(input, output)
}
pub fn strict_f64(input: &[f64], output: &mut [u8]) -> bool {
    dispatch_f64::<1>(input, output)
}
pub fn clip_f32(input: &[f32], output: &mut [u8]) -> bool {
    dispatch_f32::<2>(input, output)
}
pub fn clip_f64(input: &[f64], output: &mut [u8]) -> bool {
    dispatch_f64::<2>(input, output)
}
fn scalar_f32<const POLICY: u8>(input: &[f32], output: &mut [u8]) -> bool {
    let mut invalid = false;
    for (&value, target) in input.iter().zip(output) {
        invalid |= if POLICY == 1 {
            !((value > -1.0) & (value < 256.0))
        } else {
            !value.is_finite()
        };
        *target = if POLICY == 0 {
            scalar(value as f64)
        } else {
            value as u8
        };
    }
    invalid
}
fn scalar_f64<const POLICY: u8>(input: &[f64], output: &mut [u8]) -> bool {
    let mut invalid = false;
    for (&value, target) in input.iter().zip(output) {
        invalid |= if POLICY == 1 {
            !((value > -1.0) & (value < 256.0))
        } else {
            !value.is_finite()
        };
        *target = if POLICY == 0 {
            scalar(value)
        } else {
            value as u8
        };
    }
    invalid
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use std::arch::x86_64::*;

    #[inline]
    pub(super) fn avx512_available() -> bool {
        // Standard runtime detection includes the OS extended-state support.
        std::arch::is_x86_feature_detected!("avx512f")
            && std::arch::is_x86_feature_detected!("avx512dq")
            && std::arch::is_x86_feature_detected!("avx512bw")
            && std::arch::is_x86_feature_detected!("avx512vl")
    }
    #[target_feature(enable = "avx512f,avx512dq,avx512bw,avx512vl")]
    pub(super) unsafe fn f64_avx512<const POLICY: u8>(input: &[f64], output: &mut [u8]) -> bool {
        let mut inputs = input.chunks_exact(8);
        let mut outputs = output.chunks_exact_mut(8);
        let abs = _mm512_set1_pd(f64::from_bits(0x7fff_ffff_ffff_ffff));
        let inf = _mm512_set1_pd(f64::INFINITY);
        let mut finite = u8::MAX;
        for (input, output) in inputs.by_ref().zip(outputs.by_ref()) {
            // SAFETY: exact eight-value chunk; loadu permits unaligned reads.
            let v = unsafe { _mm512_loadu_pd(input.as_ptr()) };
            finite &= if POLICY == 1 {
                _mm512_cmp_pd_mask::<_CMP_GT_OQ>(v, _mm512_set1_pd(-1.0))
                    & _mm512_cmp_pd_mask::<_CMP_LT_OQ>(v, _mm512_set1_pd(256.0))
            } else {
                _mm512_cmp_pd_mask::<_CMP_LT_OQ>(_mm512_and_pd(v, abs), inf)
            };
            let v = if POLICY == 2 {
                _mm512_min_pd(_mm512_max_pd(v, _mm512_set1_pd(0.0)), _mm512_set1_pd(255.0))
            } else {
                v
            };
            // Invalid/out-of-i32 conversion yields INT_MIN; low byte is zero.
            let bytes = _mm256_cvtepi32_epi8(_mm512_cvttpd_epi32(v));
            output.copy_from_slice(&_mm_cvtsi128_si64(bytes).to_ne_bytes());
        }
        super::scalar_f64::<POLICY>(inputs.remainder(), outputs.into_remainder())
            | (finite != u8::MAX)
    }
    #[target_feature(enable = "avx512f,avx512dq,avx512bw,avx512vl")]
    pub(super) unsafe fn f32_avx512<const POLICY: u8>(input: &[f32], output: &mut [u8]) -> bool {
        let mut inputs = input.chunks_exact(16);
        let mut outputs = output.chunks_exact_mut(16);
        let abs = _mm512_set1_ps(f32::from_bits(0x7fff_ffff));
        let inf = _mm512_set1_ps(f32::INFINITY);
        let mut finite = u16::MAX;
        for (input, output) in inputs.by_ref().zip(outputs.by_ref()) {
            // SAFETY: exact sixteen-value chunk; unaligned reads supported.
            let v = unsafe { _mm512_loadu_ps(input.as_ptr()) };
            finite &= if POLICY == 1 {
                _mm512_cmp_ps_mask::<_CMP_GT_OQ>(v, _mm512_set1_ps(-1.0))
                    & _mm512_cmp_ps_mask::<_CMP_LT_OQ>(v, _mm512_set1_ps(256.0))
            } else {
                _mm512_cmp_ps_mask::<_CMP_LT_OQ>(_mm512_and_ps(v, abs), inf)
            };
            let v = if POLICY == 2 {
                _mm512_min_ps(_mm512_max_ps(v, _mm512_set1_ps(0.0)), _mm512_set1_ps(255.0))
            } else {
                v
            };
            let bytes = _mm512_cvtepi32_epi8(_mm512_cvttps_epi32(v));
            // SAFETY: output chunk owns exactly sixteen writable bytes; storeu
            // requires no alignment and retains no pointer beyond this call.
            unsafe { _mm_storeu_si128(output.as_mut_ptr().cast(), bytes) };
        }
        super::scalar_f32::<POLICY>(inputs.remainder(), outputs.into_remainder())
            | (finite != u16::MAX)
    }

    #[target_feature(enable = "sse2")]
    pub(super) unsafe fn f64_sse2<const POLICY: u8>(input: &[f64], output: &mut [u8]) -> bool {
        let mut inputs = input.chunks_exact(2);
        let mut outputs = output.chunks_exact_mut(2);
        let abs = _mm_set1_pd(f64::from_bits(0x7fff_ffff_ffff_ffff));
        let inf = _mm_set1_pd(f64::INFINITY);
        let mut finite = 3;
        for (input, output) in inputs.by_ref().zip(outputs.by_ref()) {
            // SAFETY: exact two-value chunk; loadu accepts unaligned addresses.
            let v = unsafe { _mm_loadu_pd(input.as_ptr()) };
            let valid = if POLICY == 1 {
                _mm_and_pd(
                    _mm_cmpgt_pd(v, _mm_set1_pd(-1.0)),
                    _mm_cmplt_pd(v, _mm_set1_pd(256.0)),
                )
            } else {
                _mm_cmplt_pd(_mm_and_pd(v, abs), inf)
            };
            finite &= _mm_movemask_pd(valid);
            let v = if POLICY == 2 {
                _mm_min_pd(_mm_max_pd(v, _mm_set1_pd(0.0)), _mm_set1_pd(255.0))
            } else {
                v
            };
            let integers = _mm_cvttpd_epi32(v);
            output[0] = _mm_cvtsi128_si32(integers) as u8;
            output[1] = _mm_cvtsi128_si32(_mm_srli_si128::<4>(integers)) as u8;
        }
        super::scalar_f64::<POLICY>(inputs.remainder(), outputs.into_remainder()) | (finite != 3)
    }
    #[target_feature(enable = "sse2")]
    pub(super) unsafe fn f32_sse2<const POLICY: u8>(input: &[f32], output: &mut [u8]) -> bool {
        let mut inputs = input.chunks_exact(4);
        let mut outputs = output.chunks_exact_mut(4);
        let abs = _mm_set1_ps(f32::from_bits(0x7fff_ffff));
        let inf = _mm_set1_ps(f32::INFINITY);
        let low_byte = _mm_set1_epi32(255);
        let mut finite = 15;
        for (input, output) in inputs.by_ref().zip(outputs.by_ref()) {
            // SAFETY: exact four-value chunk; unaligned reads are supported.
            let v = unsafe { _mm_loadu_ps(input.as_ptr()) };
            let valid = if POLICY == 1 {
                _mm_and_ps(
                    _mm_cmpgt_ps(v, _mm_set1_ps(-1.0)),
                    _mm_cmplt_ps(v, _mm_set1_ps(256.0)),
                )
            } else {
                _mm_cmplt_ps(_mm_and_ps(v, abs), inf)
            };
            finite &= _mm_movemask_ps(valid);
            let v = if POLICY == 2 {
                _mm_min_ps(_mm_max_ps(v, _mm_set1_ps(0.0)), _mm_set1_ps(255.0))
            } else {
                v
            };
            let integers = _mm_and_si128(_mm_cvttps_epi32(v), low_byte);
            // Masked values are 0..255, so signed and unsigned packs are exact.
            let words = _mm_packs_epi32(integers, integers);
            let bytes = _mm_packus_epi16(words, words);
            output.copy_from_slice(&_mm_cvtsi128_si32(bytes).to_ne_bytes());
        }
        super::scalar_f32::<POLICY>(inputs.remainder(), outputs.into_remainder()) | (finite != 15)
    }

    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn f64_avx2<const POLICY: u8>(input: &[f64], output: &mut [u8]) -> bool {
        let mut input_chunks = input.chunks_exact(4);
        let mut output_chunks = output.chunks_exact_mut(4);
        let abs_mask = _mm256_set1_pd(f64::from_bits(0x7fff_ffff_ffff_ffff));
        let infinity = _mm256_set1_pd(f64::INFINITY);
        let shuffle = _mm_setr_epi8(
            0, 4, 8, 12, -128, -128, -128, -128, -128, -128, -128, -128, -128, -128, -128, -128,
        );
        let mut finite = 15;
        for (input, output) in input_chunks.by_ref().zip(output_chunks.by_ref()) {
            // SAFETY: chunks_exact yields four readable f64 values; loadu
            // requires no alignment. No pointer is retained or written through.
            let values = unsafe { _mm256_loadu_pd(input.as_ptr()) };
            let mask = if POLICY == 1 {
                _mm256_and_pd(
                    _mm256_cmp_pd::<_CMP_GT_OQ>(values, _mm256_set1_pd(-1.0)),
                    _mm256_cmp_pd::<_CMP_LT_OQ>(values, _mm256_set1_pd(256.0)),
                )
            } else {
                _mm256_cmp_pd::<_CMP_LT_OQ>(_mm256_and_pd(values, abs_mask), infinity)
            };
            let values = if POLICY == 2 {
                _mm256_min_pd(
                    _mm256_max_pd(values, _mm256_set1_pd(0.0)),
                    _mm256_set1_pd(255.0),
                )
            } else {
                values
            };
            finite &= _mm256_movemask_pd(mask);
            // CVTT rounds toward zero. Out-of-i32-range values produce INT_MIN,
            // whose low byte is zero, matching the scalar fallback's endpoints.
            let integers = _mm256_cvttpd_epi32(values);
            let bytes = _mm_shuffle_epi8(integers, shuffle);
            output.copy_from_slice(&_mm_cvtsi128_si32(bytes).to_ne_bytes());
        }
        super::scalar_f64::<POLICY>(input_chunks.remainder(), output_chunks.into_remainder())
            | (finite != 15)
    }

    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn f32_avx2<const POLICY: u8>(input: &[f32], output: &mut [u8]) -> bool {
        let mut input_chunks = input.chunks_exact(8);
        let mut output_chunks = output.chunks_exact_mut(8);
        let abs_mask = _mm256_set1_ps(f32::from_bits(0x7fff_ffff));
        let infinity = _mm256_set1_ps(f32::INFINITY);
        let shuffle = _mm256_setr_epi8(
            0, 4, 8, 12, -128, -128, -128, -128, -128, -128, -128, -128, -128, -128, -128, -128, 0,
            4, 8, 12, -128, -128, -128, -128, -128, -128, -128, -128, -128, -128, -128, -128,
        );
        let mut finite = 255;
        for (input, output) in input_chunks.by_ref().zip(output_chunks.by_ref()) {
            // SAFETY: eight readable f32 values, with unaligned loads allowed.
            let values = unsafe { _mm256_loadu_ps(input.as_ptr()) };
            let mask = if POLICY == 1 {
                _mm256_and_ps(
                    _mm256_cmp_ps::<_CMP_GT_OQ>(values, _mm256_set1_ps(-1.0)),
                    _mm256_cmp_ps::<_CMP_LT_OQ>(values, _mm256_set1_ps(256.0)),
                )
            } else {
                _mm256_cmp_ps::<_CMP_LT_OQ>(_mm256_and_ps(values, abs_mask), infinity)
            };
            let values = if POLICY == 2 {
                _mm256_min_ps(
                    _mm256_max_ps(values, _mm256_set1_ps(0.0)),
                    _mm256_set1_ps(255.0),
                )
            } else {
                values
            };
            finite &= _mm256_movemask_ps(mask);
            let integers = _mm256_cvttps_epi32(values);
            let bytes = _mm256_shuffle_epi8(integers, shuffle);
            let packed = _mm_unpacklo_epi32(
                _mm256_castsi256_si128(bytes),
                _mm256_extracti128_si256::<1>(bytes),
            );
            output.copy_from_slice(&_mm_cvtsi128_si64(packed).to_ne_bytes());
        }
        super::scalar_f32::<POLICY>(input_chunks.remainder(), output_chunks.into_remainder())
            | (finite != 255)
    }
}

#[cfg(target_arch = "aarch64")]
mod arm {
    use std::arch::aarch64::*;

    // The original two-lane Strict f64 loop beats both the revised packed
    // kernel and LLVM's loop on native Linux ARM and Apple Silicon. Keep only
    // this measured specialization, not a duplicate full policy implementation.
    #[target_feature(enable = "neon")]
    unsafe fn strict_f64_neon(input: &[f64], output: &mut [u8]) -> bool {
        let mut inputs = input.chunks_exact(2);
        let mut outputs = output.chunks_exact_mut(2);
        let mut finite = u64::MAX;
        for (input, output) in inputs.by_ref().zip(outputs.by_ref()) {
            // SAFETY: exact two-value chunk and unaligned loads permitted.
            let v = unsafe { vld1q_f64(input.as_ptr()) };
            let valid = vandq_u64(
                vcgtq_f64(v, vdupq_n_f64(-1.0)),
                vcltq_f64(v, vdupq_n_f64(256.0)),
            );
            finite &= vgetq_lane_u64::<0>(valid) & vgetq_lane_u64::<1>(valid);
            let integers = vreinterpretq_u64_s64(vcvtq_s64_f64(v));
            output[0] = vgetq_lane_u64::<0>(integers) as u8;
            output[1] = vgetq_lane_u64::<1>(integers) as u8;
        }
        super::scalar_f64::<1>(inputs.remainder(), outputs.into_remainder()) | (finite != u64::MAX)
    }

    #[target_feature(enable = "neon")]
    pub(super) unsafe fn f64_neon<const POLICY: u8>(input: &[f64], output: &mut [u8]) -> bool {
        if POLICY == 1 {
            // SAFETY: same NEON feature and slice-length contract as this kernel.
            return unsafe { strict_f64_neon(input, output) };
        }
        let mut inputs = input.chunks_exact(2);
        let mut outputs = output.chunks_exact_mut(2);
        let lower = vdupq_n_f64(-2147483648.0);
        let upper = vdupq_n_f64(2147483648.0);
        let inf = vdupq_n_f64(f64::INFINITY);
        let mut finite = vdupq_n_u64(u64::MAX);
        for (input, output) in inputs.by_ref().zip(outputs.by_ref()) {
            // SAFETY: exact two-value chunk; AArch64 loads permit unaligned data.
            let v = unsafe { vld1q_f64(input.as_ptr()) };
            let valid = if POLICY == 1 {
                vandq_u64(
                    vcgtq_f64(v, vdupq_n_f64(-1.0)),
                    vcltq_f64(v, vdupq_n_f64(256.0)),
                )
            } else {
                vcltq_f64(vabsq_f64(v), inf)
            };
            let v = if POLICY == 2 {
                vminq_f64(vmaxq_f64(v, vdupq_n_f64(0.0)), vdupq_n_f64(255.0))
            } else {
                v
            };
            finite = vandq_u64(finite, valid);
            // NEON FCVTZS saturates. Explicit masks preserve the established
            // zero byte outside i32 range, including NaN/infinity endpoints.
            let range = if POLICY == 0 {
                vandq_u64(vcgeq_f64(v, lower), vcltq_f64(v, upper))
            } else {
                vdupq_n_u64(u64::MAX)
            };
            let integers = vandq_u64(vreinterpretq_u64_s64(vcvtq_s64_f64(v)), range);
            let words32 = vmovn_u64(integers);
            let words16 = vmovn_u32(vcombine_u32(words32, words32));
            let bytes = vmovn_u16(vcombine_u16(words16, words16));
            output.copy_from_slice(&vget_lane_u16::<0>(vreinterpret_u16_u8(bytes)).to_ne_bytes());
        }
        super::scalar_f64::<POLICY>(inputs.remainder(), outputs.into_remainder())
            | ((vgetq_lane_u64::<0>(finite) & vgetq_lane_u64::<1>(finite)) != u64::MAX)
    }
    #[target_feature(enable = "neon")]
    pub(super) unsafe fn f32_neon<const POLICY: u8>(input: &[f32], output: &mut [u8]) -> bool {
        let mut inputs = input.chunks_exact(4);
        let mut outputs = output.chunks_exact_mut(4);
        let lower = vdupq_n_f32(-2147483648.0);
        let upper = vdupq_n_f32(2147483648.0);
        let inf = vdupq_n_f32(f32::INFINITY);
        let mut finite = vdupq_n_u32(u32::MAX);
        for (input, output) in inputs.by_ref().zip(outputs.by_ref()) {
            // SAFETY: exact four-value chunk, with unaligned reads permitted.
            let v = unsafe { vld1q_f32(input.as_ptr()) };
            let valid = if POLICY == 1 {
                vandq_u32(
                    vcgtq_f32(v, vdupq_n_f32(-1.0)),
                    vcltq_f32(v, vdupq_n_f32(256.0)),
                )
            } else {
                vcltq_f32(vabsq_f32(v), inf)
            };
            finite = vandq_u32(finite, valid);
            let v = if POLICY == 2 {
                vminq_f32(vmaxq_f32(v, vdupq_n_f32(0.0)), vdupq_n_f32(255.0))
            } else {
                v
            };
            let range = if POLICY == 0 {
                vandq_u32(vcgeq_f32(v, lower), vcltq_f32(v, upper))
            } else {
                vdupq_n_u32(u32::MAX)
            };
            let integers = vandq_u32(vreinterpretq_u32_s32(vcvtq_s32_f32(v)), range);
            let words = vmovn_u32(integers);
            let bytes = vmovn_u16(vcombine_u16(words, words));
            output.copy_from_slice(&vget_lane_u32::<0>(vreinterpret_u32_u8(bytes)).to_ne_bytes());
        }
        super::scalar_f32::<POLICY>(inputs.remainder(), outputs.into_remainder())
            | (vminvq_u32(finite) != u32::MAX)
    }
}

#[cfg(all(test, target_arch = "aarch64"))]
#[path = "../tests/neon_before.rs"]
mod neon_before;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "manual isolated conversion benchmark; no CI timing threshold"]
    fn benchmark_supported_kernels() {
        use std::{
            hint::black_box,
            time::{Duration, Instant},
        };
        type F64 = fn(&[f64], &mut [u8]) -> bool;
        type F32 = fn(&[f32], &mut [u8]) -> bool;
        let mut kernels: Vec<(&str, F64, F32)> = vec![
            ("wrap/scalar", scalar_f64::<0>, scalar_f32::<0>),
            ("wrap/dispatch", wrap_f64, wrap_f32),
            ("strict/scalar", scalar_f64::<1>, scalar_f32::<1>),
            ("strict/dispatch", strict_f64, strict_f32),
            ("clip/scalar", scalar_f64::<2>, scalar_f32::<2>),
            ("clip/dispatch", clip_f64, clip_f32),
        ];
        #[cfg(target_arch = "x86_64")]
        {
            if std::arch::is_x86_feature_detected!("sse2") {
                kernels.push((
                    "sse2",
                    |i, o| unsafe { x86::f64_sse2::<0>(i, o) },
                    |i, o| unsafe { x86::f32_sse2::<0>(i, o) },
                ));
            }
            if std::arch::is_x86_feature_detected!("avx2") {
                kernels.push((
                    "avx2",
                    |i, o| unsafe { x86::f64_avx2::<0>(i, o) },
                    |i, o| unsafe { x86::f32_avx2::<0>(i, o) },
                ));
            }
        }
        #[cfg(target_arch = "aarch64")]
        if std::arch::is_aarch64_feature_detected!("neon") {
            kernels.push((
                "neon",
                |i, o| unsafe { arm::f64_neon::<0>(i, o) },
                |i, o| unsafe { arm::f32_neon::<0>(i, o) },
            ));
        }
        #[cfg(target_arch = "x86_64")]
        if x86::avx512_available() {
            // SAFETY: feature detection and benchmark's matched slice lengths.
            kernels.push((
                "avx512",
                |i, o| unsafe { x86::f64_avx512::<0>(i, o) },
                |i, o| unsafe { x86::f32_avx512::<0>(i, o) },
            ));
        }
        #[cfg(target_arch = "aarch64")]
        if std::arch::is_aarch64_feature_detected!("neon") {
            // SAFETY: native ISA guard and equal benchmark buffer lengths.
            kernels.extend([
                (
                    "wrap/neon-before",
                    (|i, o| unsafe { neon_before::f64_neon::<0>(i, o) }) as F64,
                    (|i, o| unsafe { neon_before::f32_neon::<0>(i, o) }) as F32,
                ),
                (
                    "strict/neon-before",
                    (|i, o| unsafe { neon_before::f64_neon::<1>(i, o) }) as F64,
                    (|i, o| unsafe { neon_before::f32_neon::<1>(i, o) }) as F32,
                ),
                (
                    "clip/neon-before",
                    (|i, o| unsafe { neon_before::f64_neon::<2>(i, o) }) as F64,
                    (|i, o| unsafe { neon_before::f32_neon::<2>(i, o) }) as F32,
                ),
            ]);
        }
        #[cfg(target_arch = "x86_64")]
        {
            if std::arch::is_x86_feature_detected!("avx2") {
                // SAFETY: explicit ISA guard and benchmark's matched lengths.
                kernels.extend([
                    (
                        "strict/avx2",
                        (|i, o| unsafe { x86::f64_avx2::<1>(i, o) }) as F64,
                        (|i, o| unsafe { x86::f32_avx2::<1>(i, o) }) as F32,
                    ),
                    (
                        "clip/avx2",
                        (|i, o| unsafe { x86::f64_avx2::<2>(i, o) }) as F64,
                        (|i, o| unsafe { x86::f32_avx2::<2>(i, o) }) as F32,
                    ),
                ]);
            }
            if x86::avx512_available() {
                // SAFETY: all required features guarded and lengths equal.
                kernels.extend([
                    (
                        "strict/avx512",
                        (|i, o| unsafe { x86::f64_avx512::<1>(i, o) }) as F64,
                        (|i, o| unsafe { x86::f32_avx512::<1>(i, o) }) as F32,
                    ),
                    (
                        "clip/avx512",
                        (|i, o| unsafe { x86::f64_avx512::<2>(i, o) }) as F64,
                        (|i, o| unsafe { x86::f32_avx512::<2>(i, o) }) as F32,
                    ),
                ]);
            }
        }
        let input: Vec<f64> = (0..150000)
            .map(|i| ((i * 7919) % 256) as f64 + 0.25)
            .collect();
        let input32: Vec<f32> = input.iter().map(|&v| v as f32).collect();
        let mut out = vec![0; input.len()];
        // Rotate order each repeat so dispatch is not always measured last.
        for repeat in 0..3 {
            let rotate = (repeat * 3 + 1) % kernels.len();
            kernels.rotate_left(rotate);
            for &(name, f64_kernel, f32_kernel) in &kernels {
                for dtype in ["float64", "float32"] {
                    let start = Instant::now();
                    let mut frames = 0u64;
                    while start.elapsed() < Duration::from_secs(2) {
                        let invalid = if dtype == "float64" {
                            f64_kernel(black_box(&input), black_box(&mut out))
                        } else {
                            f32_kernel(black_box(&input32), black_box(&mut out))
                        };
                        assert!(!black_box(invalid));
                        black_box(&out);
                        frames += 1;
                    }
                    println!(
                        "{{\"scope\":\"conversion and numeric validation only; excludes owning snapshot, packing and Python\",\"backend\":\"{}\",\"dtype\":\"{}\",\"repeat\":{},\"frames\":{},\"seconds\":{},\"arch\":\"{}\"}}",
                        name,
                        dtype,
                        repeat,
                        frames,
                        start.elapsed().as_secs_f64(),
                        std::env::consts::ARCH
                    );
                }
            }
        }
    }
    fn assert_policy<const POLICY: u8>() {
        type F64 = fn(&[f64], &mut [u8]) -> bool;
        type F32 = fn(&[f32], &mut [u8]) -> bool;
        let mut kernels: Vec<(F64, F32)> = vec![
            (scalar_f64::<POLICY>, scalar_f32::<POLICY>),
            (dispatch_f64::<POLICY>, dispatch_f32::<POLICY>),
        ];
        #[cfg(target_arch = "x86_64")]
        {
            if std::arch::is_x86_feature_detected!("sse2") {
                // SAFETY: guarded ISA; tests below pass matching lengths.
                kernels.push((
                    |i, o| unsafe { x86::f64_sse2::<POLICY>(i, o) },
                    |i, o| unsafe { x86::f32_sse2::<POLICY>(i, o) },
                ));
            }
            if std::arch::is_x86_feature_detected!("avx2") {
                // SAFETY: guarded ISA; tests below pass matching lengths.
                kernels.push((
                    |i, o| unsafe { x86::f64_avx2::<POLICY>(i, o) },
                    |i, o| unsafe { x86::f32_avx2::<POLICY>(i, o) },
                ));
            }
        }
        #[cfg(target_arch = "aarch64")]
        if std::arch::is_aarch64_feature_detected!("neon") {
            // SAFETY: guarded ISA; tests below pass matching lengths.
            kernels.push((
                |i, o| unsafe { arm::f64_neon::<POLICY>(i, o) },
                |i, o| unsafe { arm::f32_neon::<POLICY>(i, o) },
            ));
        }
        #[cfg(target_arch = "x86_64")]
        if x86::avx512_available() {
            // SAFETY: all required features checked; test inputs/output lengths match.
            kernels.push((
                |i, o| unsafe { x86::f64_avx512::<POLICY>(i, o) },
                |i, o| unsafe { x86::f32_avx512::<POLICY>(i, o) },
            ));
        }
        let boundary = [
            -f64::MAX,
            -2147483649.0,
            -1.0,
            -0.999999999,
            -0.0,
            0.99999,
            127.999,
            254.999,
            255.0,
            255.99999,
            256.0,
            2147483648.0,
            f64::MAX,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ];
        let rejects = |v: f64| {
            if POLICY == 1 {
                !(v > -1.0 && v < 256.0)
            } else {
                !v.is_finite()
            }
        };
        let oracle = |v: f64| {
            if v <= 0.0 {
                0
            } else if v >= 255.0 {
                255
            } else {
                v.trunc() as u8
            }
        };
        for offset in 0..8 {
            for len in 1..=65 {
                let mut values: Vec<f64> = (0..len + offset)
                    .map(|i| ((i * 7919) % 256) as f64 + 0.25)
                    .collect();
                for &value in &boundary {
                    for lane in 0..len {
                        values[offset + lane] = value;
                        let values32: Vec<f32> = values.iter().map(|&v| v as f32).collect();
                        let input = &values[offset..];
                        let input32 = &values32[offset..];
                        let invalid = input.iter().any(|&v| rejects(v));
                        let invalid32 = input32.iter().any(|&v| rejects(v as f64));
                        for &(f64_kernel, f32_kernel) in &kernels {
                            let mut output = vec![0; len];
                            assert_eq!(f64_kernel(input, &mut output), invalid);
                            if !invalid {
                                assert_eq!(
                                    output,
                                    input.iter().map(|&v| oracle(v)).collect::<Vec<_>>()
                                );
                            }
                            assert_eq!(f32_kernel(input32, &mut output), invalid32);
                            if !invalid32 {
                                assert_eq!(
                                    output,
                                    input32
                                        .iter()
                                        .map(|&v| oracle(v as f64))
                                        .collect::<Vec<_>>()
                                );
                            }
                        }
                        values[offset + lane] = ((lane * 7919) % 256) as f64 + 0.25;
                    }
                }
            }
        }
    }
    #[test]
    fn strict_and_clip_forced_backends_preserve_boundaries_invalid_lanes_and_tails() {
        assert_policy::<1>();
        assert_policy::<2>();
    }
    #[test]
    fn independent_wrapping_oracle_covers_tails_and_unaligned_offsets() {
        let values = [
            -2147483649.0,
            -2147483648.0,
            -2147483647.0,
            -1025.9,
            -257.9,
            -256.9,
            -255.9,
            -1.9,
            -0.9,
            0.,
            0.9,
            1.9,
            254.9,
            255.9,
            256.9,
            257.9,
            1025.9,
            2147483647.0,
            2147483648.0,
            1e100,
            -1e100,
        ];
        for offset in 0..8 {
            for len in 1..=65 {
                let input: Vec<f64> = (0..len + offset)
                    .map(|i| values[i % values.len()])
                    .collect();
                let input = &input[offset..];
                let expected: Vec<u8> = input
                    .iter()
                    .map(|&v| {
                        if !(-2147483648.0..2147483648.0).contains(&v) {
                            0
                        } else {
                            (v.trunc() as i64).rem_euclid(256) as u8
                        }
                    })
                    .collect();
                let mut scalar = vec![0; len];
                let mut dispatched = vec![0; len];
                assert!(!scalar_f64::<0>(input, &mut scalar));
                assert!(!wrap_f64(input, &mut dispatched));
                assert_eq!(scalar, expected);
                assert_eq!(dispatched, expected);
                let input32: Vec<f32> = input.iter().map(|&v| v as f32).collect();
                let expected32: Vec<u8> = input32
                    .iter()
                    .map(|&v| {
                        if !(-2147483648.0..2147483648.0).contains(&(v as f64)) {
                            0
                        } else {
                            (v.trunc() as i64).rem_euclid(256) as u8
                        }
                    })
                    .collect();
                assert_eq!(
                    scalar_f32::<0>(&input32, &mut scalar),
                    input32.iter().any(|v| !v.is_finite())
                );
                assert_eq!(
                    wrap_f32(&input32, &mut dispatched),
                    input32.iter().any(|v| !v.is_finite())
                );
                assert_eq!(scalar, expected32);
                assert_eq!(dispatched, expected32);
                #[cfg(target_arch = "x86_64")]
                if std::arch::is_x86_feature_detected!("sse2") {
                    // SAFETY: checked feature and matching lengths.
                    assert!(!unsafe { x86::f64_sse2::<0>(input, &mut dispatched) });
                    assert_eq!(dispatched, expected);
                    // SAFETY: checked feature and matching lengths.
                    assert_eq!(
                        unsafe { x86::f32_sse2::<0>(&input32, &mut dispatched) },
                        input32.iter().any(|v| !v.is_finite())
                    );
                    assert_eq!(dispatched, expected32);
                }
                #[cfg(target_arch = "aarch64")]
                if std::arch::is_aarch64_feature_detected!("neon") {
                    // SAFETY: checked feature and matching lengths.
                    assert!(!unsafe { arm::f64_neon::<0>(input, &mut dispatched) });
                    assert_eq!(dispatched, expected);
                    // SAFETY: checked feature and matching lengths.
                    assert_eq!(
                        unsafe { arm::f32_neon::<0>(&input32, &mut dispatched) },
                        input32.iter().any(|v| !v.is_finite())
                    );
                    assert_eq!(dispatched, expected32);
                }
            }
        }
    }
    #[test]
    fn every_nonfinite_lane_is_rejected_including_tails() {
        for len in 1..=65 {
            for position in 0..len {
                for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                    let mut values = vec![123.75; len];
                    values[position] = bad;
                    let mut output = vec![0; len];
                    assert!(wrap_f64(&values, &mut output));
                    assert!(scalar_f64::<0>(&values, &mut output));
                    let values: Vec<f32> = values.iter().map(|&v| v as f32).collect();
                    assert!(wrap_f32(&values, &mut output));
                    assert!(scalar_f32::<0>(&values, &mut output));
                }
            }
        }
    }
    #[test]
    fn randomized_forced_backends_match_independent_integer_oracle() {
        let mut seed = 0x123456789abcdef0u64;
        for offset in 0..8 {
            let mut input = Vec::new();
            for i in 0..1025 + offset {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let value = (seed >> 32) as u32 as i32 as f64 + (seed & 0xffff) as f64 / 65536.0;
                input.push(if i % 127 == 0 {
                    f64::NAN
                } else if i % 131 == 0 {
                    f64::INFINITY
                } else if i % 137 == 0 {
                    f64::NEG_INFINITY
                } else {
                    value
                });
            }
            let input32: Vec<f32> = input.iter().map(|&v| v as f32).collect();
            let input = &input[offset..];
            let input32 = &input32[offset..];
            let expected: Vec<u8> = input
                .iter()
                .map(|&v| {
                    if !(-2147483648.0..2147483648.0).contains(&v) {
                        0
                    } else {
                        (v.trunc() as i64).rem_euclid(256) as u8
                    }
                })
                .collect();
            let expected32: Vec<u8> = input32
                .iter()
                .map(|&v| {
                    if !(-2147483648.0..2147483648.0).contains(&(v as f64)) {
                        0
                    } else {
                        (v.trunc() as i64).rem_euclid(256) as u8
                    }
                })
                .collect();
            let mut out = vec![0; input.len()];
            assert!(scalar_f64::<0>(input, &mut out));
            assert_eq!(out, expected);
            assert!(scalar_f32::<0>(input32, &mut out));
            assert_eq!(out, expected32);
            #[cfg(target_arch = "x86_64")]
            if x86::avx512_available() {
                // SAFETY: feature gate and exact lengths dominate both calls.
                assert!(unsafe { x86::f64_avx512::<0>(input, &mut out) });
                assert_eq!(out, expected);
                assert!(unsafe { x86::f32_avx512::<0>(input32, &mut out) });
                assert_eq!(out, expected32);
            }
            #[cfg(target_arch = "x86_64")]
            if std::arch::is_x86_feature_detected!("sse2") {
                // SAFETY: checked ISA and equal slice lengths.
                assert!(unsafe { x86::f64_sse2::<0>(input, &mut out) });
                assert_eq!(out, expected);
                // SAFETY: checked ISA and equal slice lengths.
                assert!(unsafe { x86::f32_sse2::<0>(input32, &mut out) });
                assert_eq!(out, expected32);
            }
            #[cfg(target_arch = "aarch64")]
            if std::arch::is_aarch64_feature_detected!("neon") {
                // SAFETY: checked ISA and equal slice lengths.
                assert!(unsafe { arm::f64_neon::<0>(input, &mut out) });
                assert_eq!(out, expected);
                // SAFETY: checked ISA and equal slice lengths.
                assert!(unsafe { arm::f32_neon::<0>(input32, &mut out) });
                assert_eq!(out, expected32);
            }
            #[cfg(target_arch = "x86_64")]
            if std::arch::is_x86_feature_detected!("avx2") {
                // SAFETY: explicit runtime guard; exact equal-length slices.
                assert!(unsafe { x86::f64_avx2::<0>(input, &mut out) });
                assert_eq!(out, expected);
                // SAFETY: same guard and matched lengths; offsets vary alignment.
                assert!(unsafe { x86::f32_avx2::<0>(input32, &mut out) });
                assert_eq!(out, expected32);
            }
        }
    }
}
