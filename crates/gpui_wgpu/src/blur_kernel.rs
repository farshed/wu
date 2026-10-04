pub(crate) const MAX_KERNEL_RADIUS: usize = 128;
pub(crate) const KERNEL_VECTORS: usize = (MAX_KERNEL_RADIUS + 1).div_ceil(4);

/// Radii above the cap return zeros; the shader then computes its own weights.
pub(crate) fn gaussian_weights(sigma: f32) -> [[f32; 4]; KERNEL_VECTORS] {
    let mut weights = [[0.0; 4]; KERNEL_VECTORS];
    let sigma = sigma.max(0.5);
    let radius = (sigma * 3.0).ceil() as usize;
    if radius > MAX_KERNEL_RADIUS {
        return weights;
    }
    let mut total = 0.0;
    for k in -(radius as i32)..=radius as i32 {
        let weight = (-(k as f32) * k as f32 / (2.0 * sigma * sigma)).exp();
        total += weight;
        let k = k.unsigned_abs() as usize;
        weights[k / 4][k % 4] = weight;
    }
    for weight in weights.iter_mut().flatten() {
        *weight /= total;
    }
    weights
}

pub(crate) fn blur_regions(
    visible: [f32; 4],
    source: [u32; 4],
    output: [u32; 2],
    sigma: f32,
) -> ([u32; 4], [u32; 4]) {
    let mut vertical = [0; 4];
    for axis in 0..2 {
        let scale = output[axis] as f32 / source[axis + 2] as f32;
        let start = ((visible[axis] - source[axis] as f32) * scale).floor() as i64 - 1;
        let end =
            ((visible[axis] + visible[axis + 2] - source[axis] as f32) * scale).ceil() as i64 + 1;
        let start = start.clamp(0, output[axis] as i64) as u32;
        let end = end.clamp(start as i64, output[axis] as i64) as u32;
        vertical[axis] = start;
        vertical[axis + 2] = end - start;
    }
    let mut horizontal = vertical;
    for axis in 0..2 {
        let halo = if axis == 0 {
            1
        } else {
            (3.0 * sigma).ceil() as u32 + 1
        };
        let start = vertical[axis].saturating_sub(halo);
        let end = vertical[axis]
            .saturating_add(vertical[axis + 2])
            .saturating_add(halo)
            .min(output[axis]);
        horizontal[axis] = start;
        horizontal[axis + 2] = end - start;
    }
    (horizontal, vertical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_cover_composite_samples_and_every_vertical_tap() {
        for source_size in [[1024, 256], [1279, 799], [17, 13]] {
            for downsample in 1..=4 {
                let output = source_size.map(|n: u32| n.div_ceil(downsample));
                for sigma in [0.5, 8.0, 16.0, 100.0] {
                    for origin in [[0.0, 0.0], [31.25, 17.75], [-20.0, -10.0]] {
                        let visible = [origin[0], origin[1], 736.5, 48.5];
                        let (h, v) = blur_regions(
                            visible,
                            [0, 0, source_size[0], source_size[1]],
                            output,
                            sigma,
                        );
                        let inside = |rect: [u32; 4], axis: usize, pixel: i64| {
                            let pixel = pixel.clamp(0, output[axis] as i64 - 1) as u32;
                            pixel >= rect[axis] && pixel < rect[axis] + rect[axis + 2]
                        };
                        for axis in 0..2 {
                            for sample in 0..=100 {
                                let point =
                                    visible[axis] + visible[axis + 2] * sample as f32 / 100.0;
                                if point < 0.0 || point >= source_size[axis] as f32 {
                                    continue;
                                }
                                let texel =
                                    point * output[axis] as f32 / source_size[axis] as f32 - 0.5;
                                assert!(inside(v, axis, texel.floor() as i64));
                                assert!(inside(v, axis, texel.floor() as i64 + 1));
                            }
                            let radius = if axis == 0 {
                                0
                            } else {
                                (3.0 * sigma).ceil() as i64
                            };
                            for pixel in v[axis]..v[axis] + v[axis + 2] {
                                for offset in -radius..=radius {
                                    assert!(inside(h, axis, pixel as i64 + offset));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn regions_are_invariant_under_snapshot_translation() {
        for offset in [[0, 0], [123, 456], [2000, 1000]] {
            assert_eq!(
                blur_regions(
                    [31.25, 17.75, 736.5, 48.5],
                    [0, 0, 1024, 256],
                    [512, 128],
                    8.0
                ),
                blur_regions(
                    [
                        31.25 + offset[0] as f32,
                        17.75 + offset[1] as f32,
                        736.5,
                        48.5
                    ],
                    [offset[0], offset[1], 1024, 256],
                    [512, 128],
                    8.0
                ),
            );
        }
    }

    #[test]
    fn cached_kernel_matches_original_convolution() {
        for sigma in [0.5, 0.75, 1.0, 3.2, 8.0, 11.0, 16.0, 32.0, 42.5] {
            let radius = (sigma * 3.0_f32).ceil() as i32;
            let weights = gaussian_weights(sigma);
            for seed in 0..100 {
                let mut expected = 0.0;
                let mut total = 0.0;
                let mut actual = 0.0;
                for k in -radius..=radius {
                    let sample = ((k * 73 + seed * 37).rem_euclid(256)) as f32 / 255.0;
                    let weight = (-(k as f32) * k as f32 / (2.0 * sigma * sigma)).exp();
                    expected += sample * weight;
                    total += weight;
                    let ix = k.unsigned_abs() as usize;
                    actual += sample * weights[ix / 4][ix % 4];
                }
                assert!((expected / total - actual).abs() < 0.000002);
            }
        }
    }

    #[test]
    fn oversized_kernel_preserves_shader_fallback() {
        assert_eq!(gaussian_weights(100.0), [[0.0; 4]; KERNEL_VECTORS]);
    }

    #[test]
    fn paired_linear_samples_preserve_kernel_at_texture_edges() {
        let texels: Vec<f32> = (0..64).map(|x| ((x * 73) % 256) as f32 / 255.0).collect();
        let sample = |x: f32| {
            let x = x.clamp(0.0, (texels.len() - 1) as f32);
            let i = x.floor() as usize;
            let a = texels[i];
            let b = texels[(i + 1).min(texels.len() - 1)];
            a + (b - a) * x.fract()
        };
        for sigma in [0.5, 0.75, 1.0, 3.2, 8.0, 11.0, 16.0, 32.0, 42.5] {
            let radius = (sigma * 3.0_f32).ceil() as i32;
            let weights = gaussian_weights(sigma);
            let weight = |k: usize| weights[k / 4][k % 4];
            for center in 0..64 {
                let center = center as f32;
                let expected: f32 = (-radius..=radius)
                    .map(|k| sample(center + k as f32) * weight(k.unsigned_abs() as usize))
                    .sum();
                let mut actual = sample(center) * weight(0);
                for k in (1..=radius as usize).step_by(2) {
                    let combined = weight(k) + weight(k + 1);
                    let offset = k as f32 + weight(k + 1) / combined;
                    actual += (sample(center + offset) + sample(center - offset)) * combined;
                }
                assert!(
                    (expected - actual).abs() < 0.000002,
                    "sigma={sigma}, center={center}: {expected} vs {actual}"
                );
            }
        }
    }
}
