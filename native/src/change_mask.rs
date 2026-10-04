//! Original f64 pixel deltas: SIMD never compares quantized channel bytes.
pub fn scalar(a: &[f64], b: &[f64], mask: &mut [bool]) -> usize {
    let mut count = 0;
    for ((a, b), changed) in a.chunks_exact(3).zip(b.chunks_exact(3)).zip(mask) {
        *changed = a != b;
        count += usize::from(*changed)
    }
    count
}
pub fn compare(a: &[f64], b: &[f64], mask: &mut [bool]) -> usize {
    assert_eq!(a.len(), b.len());
    assert_eq!(a.len(), mask.len() * 3);
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: checked CPU/OS and full matching RGB slices.
        return unsafe { avx2(a, b, mask) };
    }
    scalar(a, b, mask)
}
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn avx2(a: &[f64], b: &[f64], mask: &mut [bool]) -> usize {
    use std::arch::x86_64::*;
    let mut count = 0;
    let mut left = a.chunks_exact(12);
    let mut right = b.chunks_exact(12);
    let mut masks = mask.chunks_exact_mut(4);
    for ((a, b), mask) in left.by_ref().zip(right.by_ref()).zip(masks.by_ref()) {
        let mut bits = 0;
        for offset in [0, 4, 8] {
            // SAFETY: twelve-channel chunk contains all four-lane unaligned reads.
            let (a, b) = unsafe {
                (
                    _mm256_loadu_pd(a.as_ptr().add(offset)),
                    _mm256_loadu_pd(b.as_ptr().add(offset)),
                )
            };
            bits |= _mm256_movemask_pd(_mm256_cmp_pd::<_CMP_EQ_OQ>(a, b)) << offset;
        }
        for (i, changed) in mask.iter_mut().enumerate() {
            *changed = (bits >> (i * 3)) & 7 != 7;
            count += usize::from(*changed)
        }
    }
    count + scalar(left.remainder(), right.remainder(), masks.into_remainder())
}
#[cfg(all(test, target_arch = "aarch64"))]
#[target_feature(enable = "neon")]
unsafe fn neon(a: &[f64], b: &[f64], mask: &mut [bool]) -> usize {
    use std::arch::aarch64::*;
    let mut count = 0;
    let mut left = a.chunks_exact(6);
    let mut right = b.chunks_exact(6);
    let mut masks = mask.chunks_exact_mut(2);
    for ((a, b), mask) in left.by_ref().zip(right.by_ref()).zip(masks.by_ref()) {
        // SAFETY: each six-channel chunk contains two complete RGB pixels.
        // vld3q deinterleaves the RGB triples into two-lane R/G/B vectors.
        let (a, b) = unsafe { (vld3q_f64(a.as_ptr()), vld3q_f64(b.as_ptr())) };
        let equal = vandq_u64(
            vceqq_f64(a.0, b.0),
            vandq_u64(vceqq_f64(a.1, b.1), vceqq_f64(a.2, b.2)),
        );
        mask[0] = vgetq_lane_u64::<0>(equal) != u64::MAX;
        mask[1] = vgetq_lane_u64::<1>(equal) != u64::MAX;
        count += usize::from(mask[0]) + usize::from(mask[1]);
    }
    count + scalar(left.remainder(), right.remainder(), masks.into_remainder())
}
#[cfg(test)]
mod tests {
    use super::*;
    type Kernel = fn(&[f64], &[f64], &mut [bool]) -> usize;
    fn kernels() -> Vec<(&'static str, Kernel)> {
        let kernels: Vec<(&str, Kernel)> = vec![("compiler", scalar)];
        #[cfg(target_arch = "x86_64")]
        {
            let mut kernels = kernels;
            if std::arch::is_x86_feature_detected!("avx2") {
                kernels.push(("avx2", |a, b, m| unsafe { avx2(a, b, m) }));
            }
            kernels
        }
        #[cfg(target_arch = "aarch64")]
        {
            let mut kernels = kernels;
            if std::arch::is_aarch64_feature_detected!("neon") {
                kernels.push(("neon", |a, b, m| unsafe { neon(a, b, m) }));
            }
            kernels
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        kernels
    }
    #[test]
    fn exact_deltas_all_lanes_offsets_and_tails() {
        for (_, kernel) in kernels() {
            for pixels in 1..66 {
                for offset in 0..8 {
                    let original = vec![0.0; pixels * 3 + offset];
                    let mut altered = original.clone();
                    for channel in 0..pixels * 3 {
                        altered[offset + channel] = if channel % 2 == 0 {
                            -0.0
                        } else {
                            f64::from_bits(1)
                        };
                        let mut expected = vec![false; pixels];
                        let mut actual = expected.clone();
                        let n = scalar(&original[offset..], &altered[offset..], &mut expected);
                        assert_eq!(
                            kernel(&original[offset..], &altered[offset..], &mut actual),
                            n
                        );
                        assert_eq!(actual, expected);
                    }
                }
            }
        }
    }
    #[test]
    #[ignore = "manual isolated original-value mask measurement"]
    fn benchmark_change_masks() {
        use std::{
            hint::black_box,
            time::{Duration, Instant},
        };
        let a: Vec<_> = (0..150000).map(|i| (i % 256) as f64 + 0.5).collect();
        let mut b = a.clone();
        let mut mask = vec![false; 50000];
        for repeat in 0..3 {
            for pattern in ["static", "sparse", "dense"] {
                b.copy_from_slice(&a);
                if pattern != "static" {
                    for i in (0..b.len()).step_by(if pattern == "sparse" { 300 } else { 3 }) {
                        b[i] += 0.1
                    }
                }
                for (name, kernel) in kernels() {
                    let start = Instant::now();
                    let mut frames = 0;
                    while start.elapsed() < Duration::from_secs(2) {
                        black_box(kernel(black_box(&a), black_box(&b), black_box(&mut mask)));
                        frames += 1
                    }
                    println!(
                        "{{\"kernel\":\"mask/{name}\",\"pattern\":\"{pattern}\",\"repeat\":{repeat},\"frames\":{frames},\"seconds\":{}}}",
                        start.elapsed().as_secs_f64()
                    );
                }
            }
        }
    }
}
