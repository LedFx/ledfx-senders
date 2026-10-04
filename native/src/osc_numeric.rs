//! OSC signed-i64-domain conversion. Outputs are scratch until validation passes.
pub fn scalar(input: &[f64], output: &mut [f32]) -> bool {
    let mut invalid = false;
    for (&v, out) in input.iter().zip(output) {
        invalid |= !(-9223372036854775808.0..9223372036854775808.0).contains(&v);
        *out = ((v as i64) as f64 / 255.0) as f32;
    }
    invalid
}
pub fn convert(input: &[f64], output: &mut [f32]) -> bool {
    assert_eq!(input.len(), output.len());
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("avx512f")
            && std::arch::is_x86_feature_detected!("avx512dq")
            && std::arch::is_x86_feature_detected!("avx512bw")
            && std::arch::is_x86_feature_detected!("avx512vl")
        {
            // SAFETY: complete CPU/OS checks and matching slices above.
            return unsafe { x86::avx512(input, output) };
        }
        if std::arch::is_x86_feature_detected!("avx2") {
            // SAFETY: CPU/OS checked and matching slices above.
            return unsafe { x86::avx2(input, output) };
        }
    }
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    if std::arch::is_aarch64_feature_detected!("neon") {
        // SAFETY: runtime support checked and equal slice lengths above.
        // Native Linux ARM paired measurements favor this; Apple favors LLVM.
        return unsafe { arm::neon(input, output) };
    }
    // SSE2 lacks packed double truncation. Apple Silicon measured faster on
    // LLVM's route than the explicit two-lane candidate.
    scalar(input, output)
}
#[cfg(target_arch = "x86_64")]
mod x86 {
    use std::arch::x86_64::*;
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn avx2(input: &[f64], output: &mut [f32]) -> bool {
        let mut src = input.chunks_exact(4);
        let mut dst = output.chunks_exact_mut(4);
        let mut valid = _mm256_castsi256_pd(_mm256_set1_epi64x(-1));
        for (src, dst) in src.by_ref().zip(dst.by_ref()) {
            // SAFETY: exact vector chunks, unaligned load/store, runtime-gated ISA.
            let v = unsafe { _mm256_loadu_pd(src.as_ptr()) };
            valid = _mm256_and_pd(
                valid,
                _mm256_and_pd(
                    _mm256_cmp_pd::<_CMP_GE_OQ>(v, _mm256_set1_pd(-9223372036854775808.0)),
                    _mm256_cmp_pd::<_CMP_LT_OQ>(v, _mm256_set1_pd(9223372036854775808.0)),
                ),
            );
            let n = _mm256_round_pd::<{ _MM_FROUND_TO_ZERO | _MM_FROUND_NO_EXC }>(v);
            let n = _mm256_andnot_pd(_mm256_cmp_pd::<_CMP_EQ_OQ>(n, _mm256_setzero_pd()), n);
            let value = _mm256_cvtpd_ps(_mm256_div_pd(n, _mm256_set1_pd(255.0)));
            unsafe { _mm_storeu_ps(dst.as_mut_ptr(), value) };
        }
        let invalid = _mm256_movemask_pd(valid) != 15;
        super::scalar(src.remainder(), dst.into_remainder()) | invalid
    }
    #[target_feature(enable = "avx512f,avx512dq,avx512bw,avx512vl")]
    pub(super) unsafe fn avx512(input: &[f64], output: &mut [f32]) -> bool {
        let mut src = input.chunks_exact(8);
        let mut dst = output.chunks_exact_mut(8);
        let mut valid = 255;
        for (src, dst) in src.by_ref().zip(dst.by_ref()) {
            // SAFETY: exact vector chunks, unaligned load/store, runtime-gated ISA.
            let v = unsafe { _mm512_loadu_pd(src.as_ptr()) };
            valid &= _mm512_cmp_pd_mask::<_CMP_GE_OQ>(v, _mm512_set1_pd(-9223372036854775808.0))
                & _mm512_cmp_pd_mask::<_CMP_LT_OQ>(v, _mm512_set1_pd(9223372036854775808.0));
            let n = _mm512_roundscale_pd::<{ _MM_FROUND_TO_ZERO | _MM_FROUND_NO_EXC }>(v);
            let n =
                _mm512_maskz_mov_pd(_mm512_cmp_pd_mask::<_CMP_NEQ_OQ>(n, _mm512_setzero_pd()), n);
            let value = _mm512_cvtpd_ps(_mm512_div_pd(n, _mm512_set1_pd(255.0)));
            unsafe { _mm256_storeu_ps(dst.as_mut_ptr(), value) };
        }
        super::scalar(src.remainder(), dst.into_remainder()) | (valid != 255)
    }
}
#[cfg(all(target_arch = "aarch64", any(test, target_os = "linux")))]
mod arm {
    use std::arch::aarch64::*;
    #[target_feature(enable = "neon")]
    pub(super) unsafe fn neon(input: &[f64], output: &mut [f32]) -> bool {
        let mut src = input.chunks_exact(2);
        let mut dst = output.chunks_exact_mut(2);
        let mut valid = vdupq_n_u64(u64::MAX);
        for (src, dst) in src.by_ref().zip(dst.by_ref()) {
            // SAFETY: full two-value slices and runtime checked NEON support.
            let v = unsafe { vld1q_f64(src.as_ptr()) };
            valid = vandq_u64(
                valid,
                vandq_u64(
                    vcgeq_f64(v, vdupq_n_f64(-9223372036854775808.0)),
                    vcltq_f64(v, vdupq_n_f64(9223372036854775808.0)),
                ),
            );
            let n = vrndq_f64(v);
            let n = vreinterpretq_f64_u64(vbicq_u64(
                vreinterpretq_u64_f64(n),
                vceqq_f64(n, vdupq_n_f64(0.0)),
            ));
            let value = vcvt_f32_f64(vdivq_f64(n, vdupq_n_f64(255.0)));
            unsafe { vst1_f32(dst.as_mut_ptr(), value) };
        }
        super::scalar(src.remainder(), dst.into_remainder())
            | (vgetq_lane_u64::<0>(valid) != u64::MAX || vgetq_lane_u64::<1>(valid) != u64::MAX)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    type Kernel = fn(&[f64], &mut [f32]) -> bool;
    fn kernels() -> Vec<(&'static str, Kernel)> {
        let mut kernels: Vec<(&str, Kernel)> = vec![("scalar", scalar)];
        #[cfg(target_arch = "x86_64")]
        {
            if std::arch::is_x86_feature_detected!("avx2") {
                kernels.push(("avx2", |a, b| unsafe { x86::avx2(a, b) }));
            }
            if std::arch::is_x86_feature_detected!("avx512f")
                && std::arch::is_x86_feature_detected!("avx512dq")
                && std::arch::is_x86_feature_detected!("avx512bw")
                && std::arch::is_x86_feature_detected!("avx512vl")
            {
                kernels.push(("avx512", |a, b| unsafe { x86::avx512(a, b) }));
            }
        }
        #[cfg(target_arch = "aarch64")]
        if std::arch::is_aarch64_feature_detected!("neon") {
            kernels.push(("neon", |a, b| unsafe { arm::neon(a, b) }));
        }
        kernels
    }
    #[test]
    fn forced_available_kernels_match_original_integer_math() {
        let values = [
            -9223372036854775808.0,
            f64::from_bits(9223372036854775808.0f64.to_bits() - 1),
            -2147483648.9,
            2147483648.9,
            -255.9,
            -0.0,
            -0.99,
            0.0,
            255.99,
            256.1,
            0.5,
        ];
        for (_, kernel) in kernels() {
            for count in 0..66 {
                for offset in 0..8 {
                    let input: Vec<_> = (0..count + offset)
                        .map(|i| values[i % values.len()])
                        .collect();
                    let input = &input[offset..];
                    let mut output = vec![0.0; count];
                    assert!(!kernel(input, &mut output));
                    for (&v, out) in input.iter().zip(output) {
                        assert_eq!(
                            out.to_bits(),
                            (((v as i64) as f64 / 255.0) as f32).to_bits()
                        )
                    }
                    for lane in 0..count {
                        let mut invalid = input.to_vec();
                        for bad in [
                            f64::NAN,
                            f64::INFINITY,
                            f64::NEG_INFINITY,
                            9223372036854775808.0,
                        ] {
                            invalid[lane] = bad;
                            assert!(kernel(&invalid, &mut vec![0.0; count]))
                        }
                    }
                }
            }
        }
    }
    #[test]
    #[ignore = "manual isolated paired OSC conversion measurements"]
    fn benchmark_osc_kernels() {
        use std::{
            hint::black_box,
            time::{Duration, Instant},
        };
        let input: Vec<_> = (0..150000).map(|i| (i % 256) as f64 + 0.75).collect();
        let mut output = vec![0.0; input.len()];
        for repeat in 0..3 {
            for (name, kernel) in kernels() {
                let start = Instant::now();
                let mut frames = 0;
                while start.elapsed() < Duration::from_secs(2) {
                    black_box(kernel(black_box(&input), black_box(&mut output)));
                    frames += 1
                }
                println!(
                    "{{\"kernel\":\"osc-f64/{name}\",\"repeat\":{repeat},\"channels\":{},\"frames\":{frames},\"seconds\":{}}}",
                    input.len(),
                    start.elapsed().as_secs_f64()
                );
            }
        }
    }
}
