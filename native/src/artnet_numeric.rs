//! Original-precision RGB reorder and white arithmetic. No byte quantization.
use crate::artnet::White;

macro_rules! scalar {
    ($name:ident,$ty:ty) => {
        pub(crate) fn $name(input: &[$ty], output: &mut [$ty], order: [usize; 3], white: White) {
            let width = if white == White::None { 3 } else { 4 };
            for (rgb, out) in input.chunks_exact(3).zip(output.chunks_exact_mut(width)) {
                let w = rgb[0].min(rgb[1]).min(rgb[2]);
                for j in 0..3 {
                    out[j] = if white == White::Accurate {
                        rgb[order[j]] - w
                    } else {
                        rgb[order[j]]
                    };
                }
                if width == 4 {
                    out[3] = if white == White::Zero { 0.0 } else { w };
                }
            }
        }
    };
}
scalar!(scalar_f32, f32);
scalar!(scalar_f64, f64);

pub(crate) fn f32(input: &[f32], output: &mut [f32], order: [usize; 3], white: White) {
    // Native Linux ARM measurements win for white modes. Mac's compiler wins
    // the large cases; retain its block-SIMD route instead of a size heuristic.
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    if white != White::None && std::arch::is_aarch64_feature_detected!("neon") {
        // SAFETY: ISA guard; validated slices and complete blocks plus scalar tail.
        unsafe {
            return neon_f32(input, output, order, white);
        }
    }
    scalar_f32(input, output, order, white)
}
pub(crate) fn f64(input: &[f64], output: &mut [f64], order: [usize; 3], white: White) {
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    if matches!(white, White::Brighter | White::Accurate)
        && std::arch::is_aarch64_feature_detected!("neon")
    {
        // SAFETY: ISA guard; validated slices and complete blocks plus scalar tail.
        unsafe {
            return neon_f64(input, output, order, white);
        }
    }
    scalar_f64(input, output, order, white)
}

#[cfg(all(test, target_arch = "x86_64"))]
#[target_feature(enable = "sse2")]
pub(crate) unsafe fn sse_f32(input: &[f32], output: &mut [f32], order: [usize; 3], white: White) {
    use std::arch::x86_64::*;
    let width = if white == White::None { 3 } else { 4 };
    let pixels = input.len() / 3;
    // The extra loaded lane is never used as RGB. The last pixel uses scalar
    // code: no read beyond input, and a 3-channel store cannot cross output end.
    unsafe {
        let mask = _mm_castsi128_ps(_mm_set_epi32(0, -1, -1, -1));
        for p in 0..pixels.saturating_sub(1) {
            let rgb = _mm_loadu_ps(input.as_ptr().add(p * 3));
            let w = _mm_min_ps(
                _mm_min_ps(
                    _mm_shuffle_ps::<0>(rgb, rgb),
                    _mm_shuffle_ps::<0x55>(rgb, rgb),
                ),
                _mm_shuffle_ps::<0xaa>(rgb, rgb),
            );
            let v = if white == White::Accurate {
                _mm_sub_ps(rgb, w)
            } else {
                rgb
            };
            let v = match order {
                [0, 1, 2] => v,
                [0, 2, 1] => _mm_shuffle_ps::<0xd8>(v, v),
                [1, 0, 2] => _mm_shuffle_ps::<0xe1>(v, v),
                [1, 2, 0] => _mm_shuffle_ps::<0xc9>(v, v),
                [2, 0, 1] => _mm_shuffle_ps::<0xd2>(v, v),
                _ => _mm_shuffle_ps::<0xc6>(v, v),
            };
            let w = if white == White::Zero {
                _mm_setzero_ps()
            } else {
                w
            };
            _mm_storeu_ps(
                output.as_mut_ptr().add(p * width),
                _mm_or_ps(_mm_and_ps(mask, v), _mm_andnot_ps(mask, w)),
            );
        }
    }
    let tail = pixels.saturating_sub(1);
    scalar_f32(
        &input[tail * 3..],
        &mut output[tail * width..],
        order,
        white,
    );
}
#[cfg(all(test, target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
pub(crate) unsafe fn avx_f64(input: &[f64], output: &mut [f64], order: [usize; 3], white: White) {
    use std::arch::x86_64::*;
    let width = if white == White::None { 3 } else { 4 };
    let pixels = input.len() / 3;
    unsafe {
        for p in 0..pixels.saturating_sub(1) {
            let rgb = _mm256_loadu_pd(input.as_ptr().add(p * 3));
            let w = _mm256_min_pd(
                _mm256_min_pd(
                    _mm256_permute4x64_pd::<0>(rgb),
                    _mm256_permute4x64_pd::<0x55>(rgb),
                ),
                _mm256_permute4x64_pd::<0xaa>(rgb),
            );
            let v = if white == White::Accurate {
                _mm256_sub_pd(rgb, w)
            } else {
                rgb
            };
            let v = match order {
                [0, 1, 2] => v,
                [0, 2, 1] => _mm256_permute4x64_pd::<0xd8>(v),
                [1, 0, 2] => _mm256_permute4x64_pd::<0xe1>(v),
                [1, 2, 0] => _mm256_permute4x64_pd::<0xc9>(v),
                [2, 0, 1] => _mm256_permute4x64_pd::<0xd2>(v),
                _ => _mm256_permute4x64_pd::<0xc6>(v),
            };
            let w = if white == White::Zero {
                _mm256_setzero_pd()
            } else {
                w
            };
            _mm256_storeu_pd(
                output.as_mut_ptr().add(p * width),
                _mm256_blend_pd::<8>(v, w),
            );
        }
    }
    let tail = pixels.saturating_sub(1);
    scalar_f64(
        &input[tail * 3..],
        &mut output[tail * width..],
        order,
        white,
    );
}

#[cfg(all(target_arch = "aarch64", any(test, target_os = "linux")))]
#[target_feature(enable = "neon")]
pub(crate) unsafe fn neon_f32(input: &[f32], output: &mut [f32], order: [usize; 3], white: White) {
    use std::arch::aarch64::*;
    let width = if white == White::None { 3 } else { 4 };
    let blocks = input.len() / 12;
    unsafe {
        for block in 0..blocks {
            let rgb = vld3q_f32(input.as_ptr().add(block * 12));
            let w = vminq_f32(vminq_f32(rgb.0, rgb.1), rgb.2);
            let channels = [rgb.0, rgb.1, rgb.2];
            let mut a = channels[order[0]];
            let mut b = channels[order[1]];
            let mut c = channels[order[2]];
            if white == White::Accurate {
                a = vsubq_f32(a, w);
                b = vsubq_f32(b, w);
                c = vsubq_f32(c, w);
            }
            if width == 3 {
                vst3q_f32(output.as_mut_ptr().add(block * 12), float32x4x3_t(a, b, c));
            } else {
                vst4q_f32(
                    output.as_mut_ptr().add(block * 16),
                    float32x4x4_t(
                        a,
                        b,
                        c,
                        if white == White::Zero {
                            vdupq_n_f32(0.0)
                        } else {
                            w
                        },
                    ),
                );
            }
        }
    }
    scalar_f32(
        &input[blocks * 12..],
        &mut output[blocks * 4 * width..],
        order,
        white,
    );
}
#[cfg(all(target_arch = "aarch64", any(test, target_os = "linux")))]
#[target_feature(enable = "neon")]
pub(crate) unsafe fn neon_f64(input: &[f64], output: &mut [f64], order: [usize; 3], white: White) {
    use std::arch::aarch64::*;
    let width = if white == White::None { 3 } else { 4 };
    let blocks = input.len() / 6;
    unsafe {
        for block in 0..blocks {
            let rgb = vld3q_f64(input.as_ptr().add(block * 6));
            let w = vminq_f64(vminq_f64(rgb.0, rgb.1), rgb.2);
            let channels = [rgb.0, rgb.1, rgb.2];
            let mut a = channels[order[0]];
            let mut b = channels[order[1]];
            let mut c = channels[order[2]];
            if white == White::Accurate {
                a = vsubq_f64(a, w);
                b = vsubq_f64(b, w);
                c = vsubq_f64(c, w);
            }
            if width == 3 {
                vst3q_f64(output.as_mut_ptr().add(block * 6), float64x2x3_t(a, b, c));
            } else {
                vst4q_f64(
                    output.as_mut_ptr().add(block * 8),
                    float64x2x4_t(
                        a,
                        b,
                        c,
                        if white == White::Zero {
                            vdupq_n_f64(0.0)
                        } else {
                            w
                        },
                    ),
                );
            }
        }
    }
    scalar_f64(
        &input[blocks * 6..],
        &mut output[blocks * 2 * width..],
        order,
        white,
    );
}

pub(crate) fn scalar_u8(input: &[u8], output: &mut [u8], order: [usize; 3], white: White) {
    let width = if white == White::None { 3 } else { 4 };
    for (rgb, out) in input.chunks_exact(3).zip(output.chunks_exact_mut(width)) {
        let w = rgb[0].min(rgb[1]).min(rgb[2]);
        for j in 0..3 {
            out[j] = if white == White::Accurate {
                rgb[order[j]] - w
            } else {
                rgb[order[j]]
            };
        }
        if width == 4 {
            out[3] = if white == White::Zero { 0 } else { w };
        }
    }
}
pub(crate) fn u8(input: &[u8], output: &mut [u8], order: [usize; 3], white: White) {
    if order == [0, 1, 2] && white == White::None {
        output.copy_from_slice(input);
        return;
    }
    #[cfg(target_arch = "x86_64")]
    if is_x86_feature_detected!("ssse3") {
        // SAFETY: runtime ISA guard; validated RGB/output lengths; bounded loads
        // and stores in kernel leave the short tail to the portable path.
        unsafe {
            return ssse3_u8(input, output, order, white);
        }
    }
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    if white != White::None && std::arch::is_aarch64_feature_detected!("neon") {
        // SAFETY: ISA guard; validated slices and complete blocks plus scalar tail.
        unsafe {
            return neon_u8(input, output, order, white);
        }
    }
    scalar_u8(input, output, order, white)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
pub(crate) unsafe fn ssse3_u8(input: &[u8], output: &mut [u8], order: [usize; 3], white: White) {
    use std::arch::x86_64::*;
    let width = if white == White::None { 3 } else { 4 };
    let pixels = input.len() / 3;
    // Each 16-byte load covers 12 consumed bytes and four safe lookahead bytes.
    // RGB-only stores similarly overlap four bytes overwritten by the next block/tail.
    let blocks = pixels.saturating_sub(2) / 4;
    unsafe {
        let mut indices = [-1i8; 16];
        for p in 0..4 {
            for j in 0..3 {
                indices[p * 4 + j] = (p * 3 + order[j]) as i8;
            }
        }
        let shuffle = _mm_loadu_si128(indices.as_ptr().cast());
        let expand_min = _mm_setr_epi8(0, 0, 0, 0, 4, 4, 4, 4, 8, 8, 8, 8, 12, 12, 12, 12);
        let keep_rgb = _mm_set1_epi32(0x00ffffff);
        let compact = _mm_setr_epi8(0, 1, 2, 4, 5, 6, 8, 9, 10, 12, 13, 14, -1, -1, -1, -1);
        for block in 0..blocks {
            let rgb = _mm_shuffle_epi8(
                _mm_loadu_si128(input.as_ptr().add(block * 12).cast()),
                shuffle,
            );
            let w = _mm_shuffle_epi8(
                _mm_min_epu8(
                    _mm_min_epu8(rgb, _mm_srli_epi32::<8>(rgb)),
                    _mm_srli_epi32::<16>(rgb),
                ),
                expand_min,
            );
            let v = if white == White::Accurate {
                _mm_sub_epi8(rgb, w)
            } else {
                rgb
            };
            let v = if width == 3 {
                _mm_shuffle_epi8(v, compact)
            } else {
                _mm_or_si128(
                    _mm_and_si128(keep_rgb, v),
                    _mm_andnot_si128(
                        keep_rgb,
                        if white == White::Zero {
                            _mm_setzero_si128()
                        } else {
                            w
                        },
                    ),
                )
            };
            _mm_storeu_si128(output.as_mut_ptr().add(block * 4 * width).cast(), v);
        }
    }
    scalar_u8(
        &input[blocks * 12..],
        &mut output[blocks * 4 * width..],
        order,
        white,
    );
}
#[cfg(all(target_arch = "aarch64", any(test, target_os = "linux")))]
#[target_feature(enable = "neon")]
pub(crate) unsafe fn neon_u8(input: &[u8], output: &mut [u8], order: [usize; 3], white: White) {
    use std::arch::aarch64::*;
    let width = if white == White::None { 3 } else { 4 };
    let blocks = input.len() / 48;
    unsafe {
        for block in 0..blocks {
            let rgb = vld3q_u8(input.as_ptr().add(block * 48));
            let w = vminq_u8(vminq_u8(rgb.0, rgb.1), rgb.2);
            let channels = [rgb.0, rgb.1, rgb.2];
            let mut a = channels[order[0]];
            let mut b = channels[order[1]];
            let mut c = channels[order[2]];
            if white == White::Accurate {
                a = vsubq_u8(a, w);
                b = vsubq_u8(b, w);
                c = vsubq_u8(c, w);
            }
            if width == 3 {
                vst3q_u8(output.as_mut_ptr().add(block * 48), uint8x16x3_t(a, b, c));
            } else {
                vst4q_u8(
                    output.as_mut_ptr().add(block * 64),
                    uint8x16x4_t(
                        a,
                        b,
                        c,
                        if white == White::Zero {
                            vdupq_n_u8(0)
                        } else {
                            w
                        },
                    ),
                );
            }
        }
    }
    scalar_u8(
        &input[blocks * 48..],
        &mut output[blocks * 16 * width..],
        order,
        white,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{hint::black_box, time::Instant};
    fn candidate_f32(input: &[f32], output: &mut [f32], order: [usize; 3], white: White) {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            return sse_f32(input, output, order, white);
        }
        #[cfg(target_arch = "aarch64")]
        unsafe {
            return neon_f32(input, output, order, white);
        }
        #[allow(unreachable_code)]
        scalar_f32(input, output, order, white)
    }
    fn candidate_f64(input: &[f64], output: &mut [f64], order: [usize; 3], white: White) {
        #[cfg(target_arch = "x86_64")]
        if is_x86_feature_detected!("avx2") {
            unsafe {
                return avx_f64(input, output, order, white);
            }
        }
        #[cfg(target_arch = "aarch64")]
        unsafe {
            return neon_f64(input, output, order, white);
        }
        #[allow(unreachable_code)]
        scalar_f64(input, output, order, white)
    }
    fn candidate_u8(input: &[u8], output: &mut [u8], order: [usize; 3], white: White) {
        #[cfg(target_arch = "x86_64")]
        if is_x86_feature_detected!("ssse3") {
            unsafe {
                return ssse3_u8(input, output, order, white);
            }
        }
        #[cfg(target_arch = "aarch64")]
        unsafe {
            return neon_u8(input, output, order, white);
        }
        #[allow(unreachable_code)]
        scalar_u8(input, output, order, white)
    }
    #[test]
    fn all_orders_modes_offsets_and_tails() {
        for order in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            for white in [White::None, White::Zero, White::Brighter, White::Accurate] {
                for pixels in [0, 1, 2, 3, 4, 5, 7, 8, 9, 31, 170, 171] {
                    for offset in 0..4 {
                        let input: Vec<f64> = (0..pixels * 3 + offset)
                            .map(|i| ((i * 71 % 523) as f64 - 100.0) * 0.875)
                            .collect();
                        let input = &input[offset..];
                        let width = if white == White::None { 3 } else { 4 };
                        let mut a = vec![f64::NAN; pixels * width + offset];
                        let mut b = a.clone();
                        scalar_f64(input, &mut a[offset..], order, white);
                        candidate_f64(input, &mut b[offset..], order, white);
                        assert_eq!(&a[offset..], &b[offset..]);
                        assert!(b[..offset].iter().all(|x| x.is_nan()));
                        let input: Vec<f32> = input.iter().map(|x| *x as f32).collect();
                        let mut a = vec![f32::NAN; pixels * width + offset];
                        let mut b = a.clone();
                        scalar_f32(&input, &mut a[offset..], order, white);
                        candidate_f32(&input, &mut b[offset..], order, white);
                        assert_eq!(&a[offset..], &b[offset..]);
                        assert!(b[..offset].iter().all(|x| x.is_nan()));
                        let input: Vec<u8> = (0..pixels * 3 + offset)
                            .map(|i| (i * 71 % 256) as u8)
                            .collect();
                        let mut a = vec![199; pixels * width + offset];
                        let mut b = a.clone();
                        scalar_u8(&input[offset..], &mut a[offset..], order, white);
                        candidate_u8(&input[offset..], &mut b[offset..], order, white);
                        assert_eq!(a, b);
                    }
                }
            }
        }
    }
    #[test]
    #[ignore]
    fn benchmark_artnet_transforms() {
        for pixels in [170, 50_000] {
            for white in [White::None, White::Zero, White::Brighter, White::Accurate] {
                let width = if white == White::None { 3 } else { 4 };
                let input: Vec<f64> = (0..pixels * 3)
                    .map(|i| ((i * 71 % 523) as f64 - 100.0) * 0.875)
                    .collect();
                let small: Vec<f32> = input.iter().map(|x| *x as f32).collect();
                let mut output = vec![0.0; pixels * width];
                let mut out32 = vec![0.0; pixels * width];
                let bytes: Vec<u8> = (0..pixels * 3).map(|i| (i * 71 % 256) as u8).collect();
                let mut out8 = vec![0; pixels * width];
                let n = if pixels == 170 { 10000 } else { 100 };
                for trial in 0..7 {
                    // Reverse matched order across repeats; keep every raw result.
                    for candidate in if trial % 2 == 0 {
                        [false, true]
                    } else {
                        [true, false]
                    } {
                        let f = if candidate { candidate_u8 } else { scalar_u8 };
                        let start = Instant::now();
                        for _ in 0..n {
                            f(black_box(&bytes), black_box(&mut out8), [2, 0, 1], white);
                        }
                        println!(
                            "artnet-transform pixels={pixels} kind=u8 white={} trial={trial} candidate={candidate} ns={}",
                            white as u8,
                            start.elapsed().as_nanos() / n
                        );
                        let f = if candidate { candidate_f64 } else { scalar_f64 };
                        let start = Instant::now();
                        for _ in 0..n {
                            f(black_box(&input), black_box(&mut output), [2, 0, 1], white);
                        }
                        println!(
                            "artnet-transform pixels={pixels} kind=f64 white={} trial={trial} candidate={candidate} ns={}",
                            white as u8,
                            start.elapsed().as_nanos() / n
                        );
                        let f = if candidate { candidate_f32 } else { scalar_f32 };
                        let start = Instant::now();
                        for _ in 0..n {
                            f(black_box(&small), black_box(&mut out32), [2, 0, 1], white);
                        }
                        println!(
                            "artnet-transform pixels={pixels} kind=f32 white={} trial={trial} candidate={candidate} ns={}",
                            white as u8,
                            start.elapsed().as_nanos() / n
                        );
                    }
                }
            }
        }
    }
}
