//! The pane's Lanczos3 resize, following `image::imageops::resize` a row at a time.
//!
//! `image` samples the columns of the whole picture into an `f32` image first (16 bytes a pixel:
//! about 31 MB for a 35-column pane), then the rows of that. Each output row needs only its own
//! row of the first pass, so here the first pass is one row long and is used again for every row.
//! The weights and accumulation order follow image 0.25 (`imageops/sample.rs`), with `libm` sine
//! for identical rounding across system math libraries.
use image::RgbaImage;

/// The Lanczos window of 3, as `image` computes it.
fn lanczos3(x: f32) -> f32 {
    fn sinc(t: f32) -> f32 {
        let a = t * std::f32::consts::PI;
        if t == 0.0 {
            1.0
        } else {
            libm::sinf(a) / a
        }
    }
    if x.abs() < 3.0 {
        sinc(x) * sinc(x / 3.0)
    } else {
        0.0
    }
}

/// For each output place along one axis of `from` source places: the first source place it reads
/// and the normalised weights of those it reads, as `image`'s `vertical_sample` and
/// `horizontal_sample` find them.
fn weights(from: u32, to: u32) -> Vec<(u32, Vec<f32>)> {
    let ratio = from as f32 / to as f32;
    let sratio = if ratio < 1.0 { 1.0 } else { ratio };
    let support = 3.0 * sratio;
    (0..to)
        .map(|out| {
            let input = (out as f32 + 0.5) * ratio;
            let left = ((input - support).floor() as i64).clamp(0, i64::from(from) - 1) as u32;
            let right = ((input + support).ceil() as i64)
                .clamp(i64::from(left) + 1, i64::from(from)) as u32;
            let input = input - 0.5;
            let mut ws = Vec::with_capacity((right - left) as usize);
            let mut sum = 0.0;
            for i in left..right {
                let w = lanczos3((i as f32 - input) / sratio);
                ws.push(w);
                sum += w;
            }
            for w in ws.iter_mut() {
                *w /= sum;
            }
            (left, ws)
        })
        .collect()
}

/// `image`'s clamp, which lets a NaN through as the crate's does.
fn clamp(a: f32, min: f32, max: f32) -> f32 {
    if a < min {
        min
    } else if a > max {
        max
    } else {
        a
    }
}

/// Resize RGBA with Lanczos3, using one row of `f32` scratch space.
pub fn lanczos3_resize(
    src: &[u8],
    (width, height): (u32, u32),
    (to_width, to_height): (u32, u32),
) -> Option<RgbaImage> {
    if src.len() != width as usize * height as usize * 4 {
        return None;
    }
    if width == 0 || height == 0 {
        return Some(RgbaImage::new(to_width, to_height));
    }
    if (to_width, to_height) == (width, height) {
        return RgbaImage::from_raw(width, height, src.to_vec());
    }
    let columns = weights(width, to_width);
    let stride = to_width as usize * 4;
    // Every byte of it is written below, so a buffer kept from the last frame needs no clearing.
    let mut out = super::bufpool::take(stride * to_height as usize);
    let mut row = vec![[0f32; 4]; width as usize];
    let pixel = |x: u32, y: u32| {
        let at = (y as usize * width as usize + x as usize) * 4;
        &src[at..at + 4]
    };
    for (outy, (top, ws)) in weights(height, to_height).into_iter().enumerate() {
        for (x, sum) in row.iter_mut().enumerate() {
            let mut t = (0.0, 0.0, 0.0, 0.0);
            for (i, w) in ws.iter().enumerate() {
                let p = pixel(x as u32, top + i as u32);
                t.0 += f32::from(p[0]) * w;
                t.1 += f32::from(p[1]) * w;
                t.2 += f32::from(p[2]) * w;
                t.3 += f32::from(p[3]) * w;
            }
            *sum = [t.0, t.1, t.2, t.3];
        }
        let line = &mut out[outy * stride..][..stride];
        for ((left, ws), dst) in columns.iter().zip(line.as_chunks_mut::<4>().0) {
            let mut t = (0.0, 0.0, 0.0, 0.0);
            for (i, w) in ws.iter().enumerate() {
                let p = row[*left as usize + i];
                t.0 += p[0] * w;
                t.1 += p[1] * w;
                t.2 += p[2] * w;
                t.3 += p[3] * w;
            }
            for (d, v) in dst.iter_mut().zip([t.0, t.1, t.2, t.3]) {
                // `FloatNearest`: round, then the cast `NumCast` makes of a value in range.
                *d = clamp(v, 0.0, 255.0).round() as u8;
            }
        }
    }
    RgbaImage::from_raw(to_width, to_height, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::imageops::{resize, FilterType};

    /// A picture with edges, gradients, transparency and noise in it.
    fn picture(width: u32, height: u32, seed: u32) -> RgbaImage {
        let mut state = seed.wrapping_mul(2_654_435_761).max(1);
        RgbaImage::from_fn(width, height, |x, y| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let noise = (state & 0xff) as u8;
            let edge = if (x / 7 + y / 5) % 2 == 0 { 255 } else { 0 };
            image::Rgba([
                edge,
                (x * 255 / width.max(1)) as u8,
                noise,
                if y % 9 == 0 {
                    0
                } else {
                    200u8.saturating_add(noise / 8)
                },
            ])
        })
    }

    /// The full intermediate-image algorithm from `image`, with an injectable sine function.
    fn reference_resize(src: &RgbaImage, to: (u32, u32), sin: fn(f32) -> f32) -> RgbaImage {
        if src.dimensions() == to {
            return src.clone();
        }
        let sinc = |x| {
            let a = x * std::f32::consts::PI;
            if x == 0.0 {
                1.0
            } else {
                sin(a) / a
            }
        };
        let sample = |out: u32, from: u32, to: u32, pixel: &dyn Fn(u32) -> [f32; 4]| {
            let ratio = from as f32 / to as f32;
            let scale = ratio.max(1.0);
            let center = (out as f32 + 0.5) * ratio;
            let start = ((center - 3.0 * scale).floor() as i64).clamp(0, i64::from(from) - 1);
            let end = ((center + 3.0 * scale).ceil() as i64).clamp(start + 1, i64::from(from));
            let weights: Vec<_> = (start..end)
                .map(|i| {
                    let x = (i as f32 - (center - 0.5)) / scale;
                    if x.abs() < 3.0 {
                        sinc(x) * sinc(x / 3.0)
                    } else {
                        0.0
                    }
                })
                .collect();
            let total: f32 = weights.iter().sum();
            let mut channels = [0.0; 4];
            for (i, weight) in (start..end).zip(weights) {
                let weight = weight / total;
                for (channel, value) in channels.iter_mut().zip(pixel(i as u32)) {
                    *channel += value * weight;
                }
            }
            channels
        };
        let vertical = image::Rgba32FImage::from_fn(src.width(), to.1, |x, y| {
            image::Rgba(sample(y, src.height(), to.1, &|i| {
                src[(x, i)].0.map(f32::from)
            }))
        });
        RgbaImage::from_fn(to.0, to.1, |x, y| {
            image::Rgba(
                sample(x, src.width(), to.0, &|i| vertical[(i, y)].0)
                    .map(|v| v.clamp(0.0, 255.0).round() as u8),
            )
        })
    }

    #[test]
    fn the_rows_match_the_full_image_algorithm_with_libm() {
        for (from, to, seed) in [
            ((390, 700), (300, 610), 1),
            ((120, 90), (91, 89), 2),
            ((64, 64), (200, 150), 3),
            ((50, 40), (50, 17), 4),
            ((33, 77), (12, 77), 5),
            ((7, 3), (1, 1), 6),
            ((1, 1), (5, 4), 7),
            ((16, 16), (16, 16), 8),
            ((300, 200), (200, 100), 9),
        ] {
            let src = picture(from.0, from.1, seed);
            let ours = lanczos3_resize(src.as_raw(), from, to).unwrap();
            let theirs = resize(&src, to.0, to.1, FilterType::Lanczos3);
            let native_reference = reference_resize(&src, to, f32::sin);
            assert!(
                native_reference.as_raw() == theirs.as_raw(),
                "{from:?} -> {to:?} differs from image's resize"
            );
            let deterministic_reference = reference_resize(&src, to, libm::sinf);
            assert_eq!(ours.dimensions(), theirs.dimensions(), "{from:?} -> {to:?}");
            assert!(
                ours.as_raw() == deterministic_reference.as_raw(),
                "{from:?} -> {to:?} differs from the full image resize using libm"
            );
        }
    }

    #[test]
    fn a_dirty_buffer_kept_from_an_earlier_frame_changes_no_byte() {
        let src = picture(300, 200, 9);
        let theirs = reference_resize(&src, (200, 100), libm::sinf);
        super::super::bufpool::keep(vec![0xAB; 200 * 100 * 4]);
        let ours = lanczos3_resize(src.as_raw(), (300, 200), (200, 100)).unwrap();
        assert!(ours.as_raw() == theirs.as_raw());
    }

    #[test]
    fn a_buffer_of_the_wrong_size_is_refused() {
        assert!(lanczos3_resize(&[0; 12], (2, 2), (1, 1)).is_none());
        let empty = lanczos3_resize(&[], (0, 5), (3, 2)).unwrap();
        assert_eq!(empty.as_raw(), RgbaImage::new(3, 2).as_raw());
    }
}
