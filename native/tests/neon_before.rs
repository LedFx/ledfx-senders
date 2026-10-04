//! Frozen 4afccd2 NEON kernels, test-only matched optimization control.
use std::arch::aarch64::*;

#[target_feature(enable = "neon")]
pub(super) unsafe fn f64_neon<const POLICY: u8>(input: &[f64], output: &mut [u8]) -> bool {
    let mut inputs = input.chunks_exact(2);
    let mut outputs = output.chunks_exact_mut(2);
    let lower = vdupq_n_f64(-2147483648.0);
    let upper = vdupq_n_f64(2147483648.0);
    let inf = vdupq_n_f64(f64::INFINITY);
    let mut finite = u64::MAX;
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
        finite &= vgetq_lane_u64::<0>(valid) & vgetq_lane_u64::<1>(valid);
        // NEON FCVTZS saturates. Explicit masks preserve the established
        // zero byte outside i32 range, including NaN/infinity endpoints.
        let range = if POLICY == 0 {
            vandq_u64(vcgeq_f64(v, lower), vcltq_f64(v, upper))
        } else {
            vdupq_n_u64(u64::MAX)
        };
        let integers = vandq_u64(vreinterpretq_u64_s64(vcvtq_s64_f64(v)), range);
        output[0] = vgetq_lane_u64::<0>(integers) as u8;
        output[1] = vgetq_lane_u64::<1>(integers) as u8;
    }
    super::scalar_f64::<POLICY>(inputs.remainder(), outputs.into_remainder()) | (finite != u64::MAX)
}
#[target_feature(enable = "neon")]
pub(super) unsafe fn f32_neon<const POLICY: u8>(input: &[f32], output: &mut [u8]) -> bool {
    let mut inputs = input.chunks_exact(4);
    let mut outputs = output.chunks_exact_mut(4);
    let lower = vdupq_n_f32(-2147483648.0);
    let upper = vdupq_n_f32(2147483648.0);
    let inf = vdupq_n_f32(f32::INFINITY);
    let mut finite = u32::MAX;
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
        finite &= vminvq_u32(valid);
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
    super::scalar_f32::<POLICY>(inputs.remainder(), outputs.into_remainder()) | (finite != u32::MAX)
}
