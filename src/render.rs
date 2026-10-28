use crate::sim::Flake;

/// Bytes per pixel: little-endian ARGB8888, byte order `[B, G, R, A]`.
pub const BPP: usize = 4;

/// Clear `buf` to transparent, then draw each flake as an opaque white filled
/// circle of diameter `flake.size` centered at `(x, y)`, clipped to the buffer.
///
/// `buf.len() == stride * height` and `stride >= width * 4`. Pixel `(x, y)` is
/// written at offset `(y * stride + x * BPP)`.
pub fn render_frame(buf: &mut [u8], width: u32, height: u32, stride: u32, flakes: &[Flake]) {
    buf.fill(0);

    if width == 0 || height == 0 {
        return;
    }
    let w = width as i64;
    let h = height as i64;

    for f in flakes {
        let r = f.size / 2.0;
        if r <= 0.0 {
            continue;
        }

        // Integer bounding box `[ceil(x-r) ..= floor(x+r)] x [ceil(y-r) ..= floor(y+r)]`.
        // Convert through i64 so extreme/negative floats saturate instead of panicking.
        let x0 = (f.x - r).ceil() as i64;
        let x1 = (f.x + r).floor() as i64;
        let y0 = (f.y - r).ceil() as i64;
        let y1 = (f.y + r).floor() as i64;

        // Fully offscreen: skip without touching the buffer.
        if x1 < 0 || x0 >= w || y1 < 0 || y0 >= h {
            continue;
        }

        let x_start = x0.max(0) as u32;
        let x_end = x1.min(w - 1) as u32;
        let y_start = y0.max(0) as u32;
        let y_end = y1.min(h - 1) as u32;

        let r2 = r * r;
        let fx = f.x;
        let fy = f.y;
        for py in y_start..=y_end {
            let dy = py as f32 - fy;
            for px in x_start..=x_end {
                let dx = px as f32 - fx;
                if dx * dx + dy * dy <= r2 {
                    let off = (py as usize) * (stride as usize) + (px as usize) * BPP;
                    buf[off] = 255;
                    buf[off + 1] = 255;
                    buf[off + 2] = 255;
                    buf[off + 3] = 255;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::Flake;

    fn pixel(buf: &[u8], stride: u32, x: u32, y: u32) -> [u8; 4] {
        let off = (y * stride + x * 4) as usize;
        [buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]
    }

    #[test]
    fn empty_clears_to_transparent() {
        let (w, h, stride) = (4u32, 4u32, 16u32);
        let mut buf = vec![255u8; (stride * h) as usize];
        render_frame(&mut buf, w, h, stride, &[]);
        assert!(buf.iter().all(|&b| b == 0));
    }

    #[test]
    fn single_flake_draws_white_center() {
        let (w, h, stride) = (20u32, 20u32, 80u32);
        let mut buf = vec![0u8; (stride * h) as usize];
        let f = Flake {
            x: 10.0,
            y: 10.0,
            size: 4.0,
            speed: 0.0,
            drift: 0.0,
        };
        render_frame(&mut buf, w, h, stride, &[f]);
        assert_eq!(pixel(&buf, stride, 10, 10), [255, 255, 255, 255]);
        assert_eq!(pixel(&buf, stride, 15, 10), [0, 0, 0, 0]); // distance 5 > radius 2
    }

    #[test]
    fn offscreen_flake_is_clipped() {
        let (w, h, stride) = (8u32, 8u32, 32u32);
        let mut buf = vec![0u8; (stride * h) as usize];
        let f = Flake {
            x: -100.0,
            y: 2.0,
            size: 50.0,
            speed: 0.0,
            drift: 0.0,
        };
        render_frame(&mut buf, w, h, stride, &[f]); // must not panic or write OOB
        assert!(buf.iter().all(|&b| b == 0)); // fully offscreen -> nothing drawn
    }

    #[test]
    fn honors_stride_larger_than_width() {
        let (w, h, stride) = (2u32, 2u32, 16u32); // stride = 4px worth, width = 2px
        let mut buf = vec![0u8; (stride * h) as usize];
        let f = Flake {
            x: 0.0,
            y: 0.0,
            size: 2.0,
            speed: 0.0,
            drift: 0.0,
        };
        render_frame(&mut buf, w, h, stride, &[f]);
        assert_eq!(pixel(&buf, stride, 0, 0), [255, 255, 255, 255]);
        assert_eq!(pixel(&buf, stride, 1, 1), [0, 0, 0, 0]); // outside drawn circle
    }
}
