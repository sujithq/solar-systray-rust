//! Anti-aliased tray icon renderer in plain Rust (no dependencies, so `build.rs` reuses it for
//! the executable icon). Shapes are drawn in a 16-unit design space and scaled to any size.

pub type Rgb = [u8; 3];

pub const OFFLINE: Rgb = [0x80, 0x80, 0x80];
pub const NIGHT: Rgb = [0x5C, 0x6B, 0xC0];
pub const IMPORTING: Rgb = [0xE5, 0x39, 0x35];
pub const PRODUCING: Rgb = [0xFF, 0xB3, 0x00];
pub const SURPLUS: Rgb = [0x7C, 0xB3, 0x42];
pub const MAX_SURPLUS: Rgb = [0x2E, 0x7D, 0x32];
pub const GO_BADGE: Rgb = [0x00, 0xE6, 0x76];
pub const WARNING_BADGE: Rgb = [0xFF, 0x3D, 0x00];
const WHITE: Rgb = [0xFF, 0xFF, 0xFF];
const DARK: Rgb = [0x1A, 0x1A, 0x1A];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Glyph {
    Offline,
    Night,
    Importing,
    Sun,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Face {
    Symbol { glyph: Glyph, color: Rgb },
    Number { text: String, background: Rgb },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IconSpec {
    pub face: Face,
    pub go: bool,
    pub warning: bool,
}

/// Renders straight-alpha RGBA pixels, row-major from the top-left.
pub fn render(spec: &IconSpec, size: u32) -> Vec<[u8; 4]> {
    let mut canvas = Canvas::new(size.max(1) as usize);
    match &spec.face {
        Face::Symbol { glyph, color } => {
            canvas.glyph(*glyph, *color);
            if spec.warning {
                canvas.badge(12.4, 3.6, WARNING_BADGE);
            }
            if spec.go {
                canvas.badge(12.4, 12.4, GO_BADGE);
            }
        }
        Face::Number { text, background } => {
            canvas.number(text, *background);
            // Corner dots would cover the digits, so the number face uses edge bars in the same places.
            if spec.warning {
                canvas.bar(true, WARNING_BADGE);
            }
            if spec.go {
                canvas.bar(false, GO_BADGE);
            }
        }
    }
    canvas.pixels()
}

fn coverage(distance: f32) -> f32 {
    (0.5 - distance).clamp(0.0, 1.0)
}

fn segment(px: f32, py: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (pax, pay) = (px - a.0, py - a.1);
    let (bax, bay) = (b.0 - a.0, b.1 - a.1);
    let h = ((pax * bax + pay * bay) / (bax * bax + bay * bay)).clamp(0.0, 1.0);
    (pax - bax * h).hypot(pay - bay * h)
}

fn rounded_box(px: f32, py: f32, center: (f32, f32), half: f32, radius: f32) -> f32 {
    let qx = (px - center.0).abs() - half + radius;
    let qy = (py - center.1).abs() - half + radius;
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius
}

/// The number face background.
fn number_box(px: f32, py: f32) -> f32 {
    rounded_box(px, py, (8.0, 8.0), 7.8, 3.2)
}

/// Inner edge of the number face's status bars; text stays inside `BAR_EDGE..16 - BAR_EDGE`.
const BAR_EDGE: f32 = 2.6;

struct Canvas {
    n: usize,
    unit: f32,
    /// Premultiplied RGBA in 0..1.
    px: Vec<[f32; 4]>,
}

impl Canvas {
    fn new(n: usize) -> Self {
        Canvas { n, unit: n as f32 / 16.0, px: vec![[0.0; 4]; n * n] }
    }

    /// Blends `color` over each pixel with the coverage `shape` returns for a point in design units.
    fn fill(&mut self, color: Rgb, shape: impl Fn(f32, f32) -> f32) {
        let rgb = color.map(|c| c as f32 / 255.0);
        for y in 0..self.n {
            for x in 0..self.n {
                let (ux, uy) = ((x as f32 + 0.5) / self.unit, (y as f32 + 0.5) / self.unit);
                self.blend(x, y, rgb, shape(ux, uy));
            }
        }
    }

    fn blend(&mut self, x: usize, y: usize, rgb: [f32; 3], alpha: f32) {
        let alpha = alpha.clamp(0.0, 1.0);
        if alpha <= 0.0 {
            return;
        }
        let p = &mut self.px[y * self.n + x];
        for c in 0..3 {
            p[c] = rgb[c] * alpha + p[c] * (1.0 - alpha);
        }
        p[3] = alpha + p[3] * (1.0 - alpha);
    }

    fn erase(&mut self, shape: impl Fn(f32, f32) -> f32) {
        for y in 0..self.n {
            for x in 0..self.n {
                let (ux, uy) = ((x as f32 + 0.5) / self.unit, (y as f32 + 0.5) / self.unit);
                let keep = 1.0 - shape(ux, uy).clamp(0.0, 1.0);
                self.px[y * self.n + x].iter_mut().for_each(|v| *v *= keep);
            }
        }
    }

    fn glyph(&mut self, glyph: Glyph, color: Rgb) {
        // Distances are in design units; scaling them keeps the anti-aliasing band one pixel wide.
        let u = self.unit;
        let cov = move |d: f32| coverage(d * u);
        match glyph {
            Glyph::Sun => {
                self.fill(color, |x, y| cov((x - 8.0).hypot(y - 8.0) - 4.1));
                self.fill(color, |x, y| {
                    (0..8)
                        .map(|k| {
                            let angle = k as f32 * std::f32::consts::FRAC_PI_4;
                            let (s, c) = angle.sin_cos();
                            let a = (8.0 + 5.7 * c, 8.0 + 5.7 * s);
                            let b = (8.0 + 7.3 * c, 8.0 + 7.3 * s);
                            cov(segment(x, y, a, b) - 0.8)
                        })
                        .fold(0.0, f32::max)
                });
            }
            Glyph::Night => self.fill(color, |x, y| {
                let outer = cov((x - 8.0).hypot(y - 8.0) - 6.8);
                let bite = cov((x - 11.2).hypot(y - 5.0) - 5.4);
                outer * (1.0 - bite)
            }),
            Glyph::Importing => {
                self.fill(color, |x, y| cov((x - 8.0).hypot(y - 8.0) - 7.3));
                self.fill(WHITE, |x, y| {
                    let shaft = segment(x, y, (8.0, 3.6), (8.0, 11.6));
                    let left = segment(x, y, (8.0, 12.0), (4.7, 8.7));
                    let right = segment(x, y, (8.0, 12.0), (11.3, 8.7));
                    cov(shaft.min(left).min(right) - 1.0)
                });
            }
            Glyph::Offline => self.fill(color, |x, y| {
                let ring = ((x - 8.0).hypot(y - 8.0) - 5.9).abs() - 0.9;
                let slash = segment(x, y, (3.8, 12.2), (12.2, 3.8)) - 0.9;
                cov(ring.min(slash))
            }),
        }
    }

    fn number(&mut self, text: &str, background: Rgb) {
        let unit = self.unit;
        self.fill(background, |x, y| coverage(number_box(x, y) * unit));
        let luminance = 0.299 * background[0] as f32 + 0.587 * background[1] as f32 + 0.114 * background[2] as f32;
        let ink = if luminance > 160.0 { DARK } else { WHITE };
        self.text(text, ink);
    }

    /// Draws 5x7 bitmap text, centred, using exact pixel-area coverage of each font cell.
    fn text(&mut self, text: &str, ink: Rgb) {
        let glyphs: Vec<&Bitmap> = text.chars().filter_map(bitmap).collect();
        if glyphs.is_empty() {
            return;
        }
        let cells_wide = glyphs.iter().map(|g| g.width).sum::<usize>() + glyphs.len() - 1;
        let n = self.n as f32;
        // The height limit leaves room for a status bar plus its gap at the top and bottom.
        let max_height = n * (16.0 - 2.0 * (BAR_EDGE + 1.0)) / 16.0;
        let raw = (n * 0.88 / cells_wide as f32).min(max_height / 7.0);
        let scale = if raw >= 1.0 { (raw * 2.0).floor() / 2.0 } else { raw };
        let x0 = ((n - cells_wide as f32 * scale) / 2.0).round();
        let y0 = ((n - 7.0 * scale) / 2.0).round();
        let mut alpha = vec![0.0f32; self.n * self.n];
        let mut cursor = 0usize;
        for glyph in glyphs {
            for (row, bits) in glyph.rows.iter().enumerate() {
                for col in 0..glyph.width {
                    if bits >> (glyph.width - 1 - col) & 1 == 1 {
                        let left = x0 + (cursor + col) as f32 * scale;
                        let top = y0 + row as f32 * scale;
                        self.cell(&mut alpha, left, top, scale);
                    }
                }
            }
            cursor += glyph.width + 1;
        }
        let rgb = ink.map(|c| c as f32 / 255.0);
        for y in 0..self.n {
            for x in 0..self.n {
                self.blend(x, y, rgb, alpha[y * self.n + x]);
            }
        }
    }

    fn cell(&self, alpha: &mut [f32], left: f32, top: f32, size: f32) {
        let (right, bottom) = (left + size, top + size);
        let last = self.n as isize - 1;
        let span = |a: f32, b: f32| (a.floor() as isize).max(0)..=(b.ceil() as isize - 1).min(last);
        for y in span(top, bottom) {
            let h = (bottom.min(y as f32 + 1.0) - top.max(y as f32)).max(0.0);
            for x in span(left, right) {
                let w = (right.min(x as f32 + 1.0) - left.max(x as f32)).max(0.0);
                let a = &mut alpha[y as usize * self.n + x as usize];
                *a = (*a + w * h).min(1.0);
            }
        }
    }

    /// A status dot with a transparent cut-out ring, so it reads on any taskbar colour.
    fn badge(&mut self, cx: f32, cy: f32, color: Rgb) {
        let r = 3.1;
        let gap = 0.9_f32.max(1.0 / self.unit);
        let unit = self.unit;
        self.erase(|x, y| coverage(((x - cx).hypot(y - cy) - r - gap) * unit));
        self.fill(color, |x, y| coverage(((x - cx).hypot(y - cy) - r) * unit));
    }

    /// A status band across the top or bottom of the number face, cut off by a transparent gap.
    fn bar(&mut self, top: bool, color: Rgb) {
        let gap = 0.9_f32.max(1.0 / self.unit);
        let unit = self.unit;
        // Signed distance from the bar's inner edge, positive towards the centre.
        let inward = move |y: f32| if top { y - BAR_EDGE } else { 16.0 - BAR_EDGE - y };
        self.erase(|x, y| coverage(number_box(x, y).max(inward(y) - gap).max(-inward(y)) * unit));
        self.fill(color, |x, y| coverage(number_box(x, y).max(inward(y)) * unit));
    }

    fn pixels(&self) -> Vec<[u8; 4]> {
        self.px
            .iter()
            .map(|p| {
                let a = p[3];
                let channel = |v: f32| if a > 0.0 { ((v / a).clamp(0.0, 1.0) * 255.0).round() as u8 } else { 0 };
                [channel(p[0]), channel(p[1]), channel(p[2]), (a * 255.0).round() as u8]
            })
            .collect()
    }
}

struct Bitmap {
    width: usize,
    rows: [u8; 7],
}

fn bitmap(c: char) -> Option<&'static Bitmap> {
    const DIGITS: [Bitmap; 10] = [
        Bitmap { width: 5, rows: [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110] },
        Bitmap { width: 5, rows: [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110] },
        Bitmap { width: 5, rows: [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111] },
        Bitmap { width: 5, rows: [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110] },
        Bitmap { width: 5, rows: [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010] },
        Bitmap { width: 5, rows: [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110] },
        Bitmap { width: 5, rows: [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110] },
        Bitmap { width: 5, rows: [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000] },
        Bitmap { width: 5, rows: [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110] },
        Bitmap { width: 5, rows: [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100] },
    ];
    const DOT: Bitmap = Bitmap { width: 1, rows: [0, 0, 0, 0, 0, 0, 1] };
    const DASH: Bitmap = Bitmap { width: 3, rows: [0, 0, 0, 0b111, 0, 0, 0] };
    const BANG: Bitmap = Bitmap { width: 1, rows: [1, 1, 1, 1, 1, 0, 1] };
    match c {
        '0'..='9' => Some(&DIGITS[c as usize - '0' as usize]),
        '.' => Some(&DOT),
        '-' => Some(&DASH),
        '!' => Some(&BANG),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbol(glyph: Glyph, color: Rgb) -> IconSpec {
        IconSpec { face: Face::Symbol { glyph, color }, go: false, warning: false }
    }

    fn at(pixels: &[[u8; 4]], size: u32, x: u32, y: u32) -> [u8; 4] {
        pixels[(y * size + x) as usize]
    }

    #[test]
    fn renders_every_size_with_transparent_corners_and_an_opaque_centre() {
        for size in [16, 20, 24, 32, 40, 48, 64] {
            for glyph in [Glyph::Sun, Glyph::Importing, Glyph::Offline] {
                let pixels = render(&symbol(glyph, PRODUCING), size);
                assert_eq!(pixels.len(), (size * size) as usize);
                assert_eq!(at(&pixels, size, 0, 0)[3], 0, "{glyph:?} corner at {size}");
                let centre = at(&pixels, size, size / 2, size / 2)[3];
                // The offline slash is thin, so at small sizes it only partly covers the centre.
                let opaque = if glyph == Glyph::Offline { 128 } else { 255 };
                assert!(centre >= opaque, "{glyph:?} centre at {size}: {centre}");
            }
        }
    }

    #[test]
    fn sun_is_drawn_in_its_level_colour() {
        let pixels = render(&symbol(Glyph::Sun, SURPLUS), 32);
        assert_eq!(at(&pixels, 32, 16, 16), [SURPLUS[0], SURPLUS[1], SURPLUS[2], 255]);
    }

    #[test]
    fn crescent_leaves_the_bite_transparent() {
        let pixels = render(&symbol(Glyph::Night, NIGHT), 32);
        assert_eq!(at(&pixels, 32, 22, 10)[3], 0, "the bite");
        assert_eq!(at(&pixels, 32, 8, 18)[3], 255, "the body");
    }

    #[test]
    fn badges_mark_go_and_peak_warnings() {
        let spec = IconSpec { go: true, warning: true, ..symbol(Glyph::Sun, PRODUCING) };
        let pixels = render(&spec, 32);
        let go = at(&pixels, 32, 25, 25);
        assert_eq!([go[0], go[1], go[2], go[3]], [GO_BADGE[0], GO_BADGE[1], GO_BADGE[2], 255]);
        let warn = at(&pixels, 32, 25, 7);
        assert_eq!([warn[0], warn[1], warn[2], warn[3]], [WARNING_BADGE[0], WARNING_BADGE[1], WARNING_BADGE[2], 255]);
    }

    #[test]
    fn number_face_draws_crisp_ink_on_the_background() {
        let spec =
            IconSpec { face: Face::Number { text: "2.4".into(), background: MAX_SURPLUS }, go: false, warning: false };
        let pixels = render(&spec, 16);
        let ink = pixels.iter().filter(|p| p[..3] == WHITE && p[3] == 255).count();
        assert!(ink > 20, "white text pixels: {ink}");
        let light =
            IconSpec { face: Face::Number { text: "0.3".into(), background: PRODUCING }, go: false, warning: false };
        let pixels = render(&light, 16);
        assert!(pixels.iter().any(|p| p[..3] == DARK), "dark ink on yellow");
        assert_eq!(at(&pixels, 16, 0, 0)[3], 0, "rounded corner");
    }

    #[test]
    fn number_face_status_bars_leave_the_digits_intact() {
        for size in [16, 20, 24, 32, 48] {
            for text in ["12", "4.2", "99", "--"] {
                let plain = IconSpec {
                    face: Face::Number { text: text.into(), background: MAX_SURPLUS },
                    go: false,
                    warning: false,
                };
                let flagged = IconSpec { go: true, warning: true, ..plain.clone() };
                let ink = |spec: &IconSpec| -> Vec<usize> {
                    let pixels = render(spec, size);
                    (0..pixels.len()).filter(|&i| pixels[i][..3] == WHITE && pixels[i][3] > 0).collect()
                };
                assert_eq!(ink(&plain), ink(&flagged), "{text} at {size}");
            }
        }
        let flagged =
            IconSpec { face: Face::Number { text: "12".into(), background: SURPLUS }, go: true, warning: true };
        let pixels = render(&flagged, 32);
        assert_eq!(at(&pixels, 32, 16, 2), [WARNING_BADGE[0], WARNING_BADGE[1], WARNING_BADGE[2], 255]);
        assert_eq!(at(&pixels, 32, 16, 29), [GO_BADGE[0], GO_BADGE[1], GO_BADGE[2], 255]);
        assert_eq!(at(&pixels, 32, 16, 6)[3], 0, "gap below the warning bar");
    }

    #[test]
    fn text_fits_inside_the_icon() {
        for text in ["8.8", "88", "--", "0.0"] {
            for size in [16, 24, 32] {
                let spec =
                    IconSpec { face: Face::Number { text: text.into(), background: DARK }, go: false, warning: false };
                let pixels = render(&spec, size);
                let edge = (0..size).flat_map(|i| [(0, i), (size - 1, i)]);
                for (x, y) in edge {
                    assert_ne!(at(&pixels, size, x, y)[..3], WHITE, "{text} at {size} touches the edge");
                }
            }
        }
    }
}
