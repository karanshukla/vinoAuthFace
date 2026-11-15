//! Anti-aliased rounded rectangles into a premultiplied BGRA buffer, enough to
//! draw the tray icon without an image or font stack. Ported from vinoWhisper's
//! `gui/src/raster.rs`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    pub fn inset(self, by: f32) -> Rect {
        Rect {
            x0: self.x0 + by,
            y0: self.y0 + by,
            x1: self.x1 - by,
            y1: self.y1 - by,
        }
    }
}

pub struct Canvas<'a> {
    pixels: &'a mut [u8],
    width: u32,
    height: u32,
}

impl<'a> Canvas<'a> {
    pub fn new(pixels: &'a mut [u8], width: u32, height: u32) -> Self {
        assert!(
            pixels.len() >= width as usize * height as usize * 4,
            "a {width}x{height} canvas needs {} bytes, got {}",
            width as usize * height as usize * 4,
            pixels.len()
        );
        Canvas {
            pixels,
            width,
            height,
        }
    }

    pub fn blend(&mut self, x: i32, y: i32, color: Rgba, coverage: f32) {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return;
        }
        let alpha = f32::from(color.3) / 255.0 * coverage.clamp(0.0, 1.0);
        if alpha <= 0.0 {
            return;
        }
        let i = (y as usize * self.width as usize + x as usize) * 4;
        let pixel = &mut self.pixels[i..i + 4];
        let keep = 1.0 - alpha;
        let over =
            |src: u8, dst: u8| (f32::from(src) * alpha + f32::from(dst) * keep).round() as u8;
        pixel[0] = over(color.2, pixel[0]);
        pixel[1] = over(color.1, pixel[1]);
        pixel[2] = over(color.0, pixel[2]);
        pixel[3] = over(255, pixel[3]);
    }

    pub fn fill_rounded(&mut self, rect: Rect, radius: f32, color: Rgba) {
        self.cover(rect, color, |x, y| rounded_coverage(x, y, rect, radius));
    }

    pub fn stroke_rounded(&mut self, rect: Rect, radius: f32, width: f32, color: Rgba) {
        let inner = rect.inset(width);
        let inner_radius = (radius - width).max(0.0);
        self.cover(rect, color, |x, y| {
            rounded_coverage(x, y, rect, radius) - rounded_coverage(x, y, inner, inner_radius)
        });
    }

    fn cover(&mut self, rect: Rect, color: Rgba, coverage: impl Fn(f32, f32) -> f32) {
        let (x0, y0) = (rect.x0.floor() as i32, rect.y0.floor() as i32);
        let (x1, y1) = (rect.x1.ceil() as i32, rect.y1.ceil() as i32);
        for y in y0.max(0)..y1.min(self.height as i32) {
            for x in x0.max(0)..x1.min(self.width as i32) {
                let amount = coverage(x as f32 + 0.5, y as f32 + 0.5);
                if amount > 0.0 {
                    self.blend(x, y, color, amount);
                }
            }
        }
    }

    pub fn to_argb32_be(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len());
        for pixel in self.pixels.as_chunks::<4>().0 {
            let alpha = pixel[3];
            let straight = |premultiplied: u8| {
                if alpha == 0 {
                    0
                } else {
                    ((u16::from(premultiplied) * 255 + u16::from(alpha) / 2) / u16::from(alpha))
                        .min(255) as u8
                }
            };
            out.extend_from_slice(&[
                alpha,
                straight(pixel[2]),
                straight(pixel[1]),
                straight(pixel[0]),
            ]);
        }
        out
    }
}

pub fn rounded_coverage(px: f32, py: f32, rect: Rect, radius: f32) -> f32 {
    let half_w = (rect.x1 - rect.x0) / 2.0;
    let half_h = (rect.y1 - rect.y0) / 2.0;
    if half_w <= 0.0 || half_h <= 0.0 {
        return 0.0;
    }
    let r = radius.clamp(0.0, half_w.min(half_h));
    let qx = (px - (rect.x0 + half_w)).abs() - (half_w - r);
    let qy = (py - (rect.y0 + half_h)).abs() - (half_h - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    let inside = qx.max(qy).min(0.0);
    let distance = outside + inside - r;
    (0.5 - distance).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQUARE: Rect = Rect {
        x0: 0.0,
        y0: 0.0,
        x1: 10.0,
        y1: 10.0,
    };

    #[test]
    fn the_inside_is_covered_and_the_outside_is_not() {
        assert_eq!(rounded_coverage(5.0, 5.0, SQUARE, 3.0), 1.0);
        assert_eq!(rounded_coverage(20.0, 5.0, SQUARE, 3.0), 0.0);
    }

    #[test]
    fn a_square_corner_is_fully_covered_up_to_its_edge() {
        assert_eq!(rounded_coverage(0.5, 0.5, SQUARE, 0.0), 1.0);
    }

    #[test]
    fn a_rounded_corner_is_cut_away() {
        assert_eq!(rounded_coverage(0.5, 0.5, SQUARE, 4.0), 0.0);
        assert_eq!(
            rounded_coverage(5.0, 0.5, SQUARE, 4.0),
            1.0,
            "mid-edge is untouched"
        );
    }

    #[test]
    fn an_edge_straddling_pixel_is_half_covered() {
        assert!((rounded_coverage(10.0, 5.0, SQUARE, 0.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn blending_is_premultiplied_in_wayland_byte_order() {
        let mut pixels = vec![0u8; 4];
        let mut canvas = Canvas::new(&mut pixels, 1, 1);
        canvas.blend(0, 0, Rgba(255, 0, 0, 128), 1.0);
        assert_eq!(pixels, vec![0, 0, 128, 128]);
    }

    #[test]
    fn out_of_bounds_pixels_are_ignored() {
        let mut pixels = vec![0u8; 4];
        let mut canvas = Canvas::new(&mut pixels, 1, 1);
        canvas.blend(-1, 0, Rgba(255, 255, 255, 255), 1.0);
        canvas.blend(1, 0, Rgba(255, 255, 255, 255), 1.0);
        assert_eq!(pixels, vec![0, 0, 0, 0]);
    }

    #[test]
    fn a_tray_pixmap_is_straight_alpha_argb() {
        let mut pixels = vec![0, 0, 128, 128];
        let canvas = Canvas::new(&mut pixels, 1, 1);
        assert_eq!(canvas.to_argb32_be(), vec![128, 255, 0, 0]);
    }
}
