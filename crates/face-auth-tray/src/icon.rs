//! The tray and app icon, in vinoWhisper's style: a dark rounded tile with a
//! faint edge and pill-shaped marks. Here the marks are a camera viewfinder
//! around a face. Drawn in code so the tray needs no image decoder, and the
//! checked-in SVG is generated from the same shapes.

use crate::raster::{Canvas, Rect, Rgba};

#[derive(Debug, Clone, Copy)]
struct Shape {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
}

impl Shape {
    const fn new(x: f32, y: f32, w: f32, h: f32, r: f32) -> Shape {
        Shape { x, y, w, h, r }
    }

    fn inset(self, by: f32) -> Shape {
        Shape::new(self.x + by, self.y + by, self.w - by * 2.0, self.h - by * 2.0, self.r - by)
    }

    fn rect(self, unit: f32) -> Rect {
        Rect {
            x0: self.x * unit,
            y0: self.y * unit,
            x1: (self.x + self.w) * unit,
            y1: (self.y + self.h) * unit,
        }
    }

    fn svg_path(self) -> String {
        let Shape { x, y, w, h, .. } = self;
        format!("M{x} {y}h{w}v{h}h-{w}z")
    }

    fn svg_rect(self, paint: &str) -> String {
        let Shape { x, y, w, h, r } = self;
        format!(r#"<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}" {paint}/>"#)
    }
}

/// What the icon shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Enrolled, camera present.
    Ready,
    /// A `vinoauthface-auth` process is running somewhere on the machine.
    Scanning,
    /// Not usable yet: no templates, or no IR camera.
    Attention,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Bracket,
    Feature,
    ScanLine,
}

// vinoWhisper's palette (gui/src/paint.rs).
const TILE: Rgba = Rgba(14, 14, 16, 255);
const EDGE: Rgba = Rgba(255, 255, 255, 46);
const WHITE: Rgba = Rgba(250, 250, 250, 255);
const GREY: Rgba = Rgba(160, 164, 172, 255);
const GREEN: Rgba = Rgba(86, 204, 120, 255);
const AMBER: Rgba = Rgba(245, 184, 66, 255);

fn paint(state: State, part: Part) -> Option<Rgba> {
    match (state, part) {
        (State::Ready, Part::Bracket) => Some(WHITE),
        (State::Ready, Part::Feature) => Some(GREEN),
        (State::Scanning, Part::Bracket | Part::ScanLine) => Some(AMBER),
        (State::Scanning, Part::Feature) => Some(WHITE),
        (State::Attention, Part::Bracket | Part::Feature) => Some(GREY),
        (State::Ready | State::Attention, Part::ScanLine) => None,
    }
}

struct Grid {
    tile: Shape,
    edge: f32,
    marks: &'static [(Shape, Part)],
    size: f32,
}

/// 64-unit design, for the app icon and the larger tray sizes.
const LARGE: Grid = Grid {
    tile: Shape::new(4.0, 4.0, 56.0, 56.0, 12.0),
    edge: 2.0,
    marks: &[
        (Shape::new(12.0, 12.0, 12.0, 4.0, 2.0), Part::Bracket),
        (Shape::new(12.0, 12.0, 4.0, 12.0, 2.0), Part::Bracket),
        (Shape::new(40.0, 12.0, 12.0, 4.0, 2.0), Part::Bracket),
        (Shape::new(48.0, 12.0, 4.0, 12.0, 2.0), Part::Bracket),
        (Shape::new(12.0, 48.0, 12.0, 4.0, 2.0), Part::Bracket),
        (Shape::new(12.0, 40.0, 4.0, 12.0, 2.0), Part::Bracket),
        (Shape::new(40.0, 48.0, 12.0, 4.0, 2.0), Part::Bracket),
        (Shape::new(48.0, 40.0, 4.0, 12.0, 2.0), Part::Bracket),
        (Shape::new(23.0, 22.0, 5.0, 9.0, 2.5), Part::Feature),
        (Shape::new(36.0, 22.0, 5.0, 9.0, 2.5), Part::Feature),
        (Shape::new(24.0, 39.0, 16.0, 5.0, 2.5), Part::Feature),
        (Shape::new(18.0, 34.0, 28.0, 2.0, 1.0), Part::ScanLine),
    ],
    size: 64.0,
};

/// 16-unit design on whole pixels, so the 16 px tray icon stays sharp.
const SMALL: Grid = Grid {
    tile: Shape::new(1.0, 1.0, 14.0, 14.0, 3.0),
    edge: 1.0,
    marks: &[
        (Shape::new(3.0, 3.0, 3.0, 1.0, 0.0), Part::Bracket),
        (Shape::new(3.0, 3.0, 1.0, 3.0, 0.0), Part::Bracket),
        (Shape::new(10.0, 3.0, 3.0, 1.0, 0.0), Part::Bracket),
        (Shape::new(12.0, 3.0, 1.0, 3.0, 0.0), Part::Bracket),
        (Shape::new(3.0, 12.0, 3.0, 1.0, 0.0), Part::Bracket),
        (Shape::new(3.0, 10.0, 1.0, 3.0, 0.0), Part::Bracket),
        (Shape::new(10.0, 12.0, 3.0, 1.0, 0.0), Part::Bracket),
        (Shape::new(12.0, 10.0, 1.0, 3.0, 0.0), Part::Bracket),
        (Shape::new(6.0, 5.0, 1.0, 2.0, 0.0), Part::Feature),
        (Shape::new(9.0, 5.0, 1.0, 2.0, 0.0), Part::Feature),
        (Shape::new(6.0, 10.0, 4.0, 1.0, 0.0), Part::Feature),
        (Shape::new(5.0, 8.0, 6.0, 1.0, 0.0), Part::ScanLine),
    ],
    size: 16.0,
};

/// 16-unit design for the symbolic tray icon, which has no tile: the
/// viewfinder sits on a 12-unit square with a 2-unit margin, the same
/// footprint as the other icons in the tray.
const SYMBOLIC: Grid = Grid {
    tile: Shape::new(0.0, 0.0, 16.0, 16.0, 0.0),
    edge: 0.0,
    marks: &[
        (Shape::new(2.0, 2.0, 3.0, 1.0, 0.0), Part::Bracket),
        (Shape::new(2.0, 2.0, 1.0, 3.0, 0.0), Part::Bracket),
        (Shape::new(11.0, 2.0, 3.0, 1.0, 0.0), Part::Bracket),
        (Shape::new(13.0, 2.0, 1.0, 3.0, 0.0), Part::Bracket),
        (Shape::new(2.0, 13.0, 3.0, 1.0, 0.0), Part::Bracket),
        (Shape::new(2.0, 11.0, 1.0, 3.0, 0.0), Part::Bracket),
        (Shape::new(11.0, 13.0, 3.0, 1.0, 0.0), Part::Bracket),
        (Shape::new(13.0, 11.0, 1.0, 3.0, 0.0), Part::Bracket),
        (Shape::new(5.0, 5.0, 2.0, 2.0, 0.0), Part::Feature),
        (Shape::new(9.0, 5.0, 2.0, 2.0, 0.0), Part::Feature),
        (Shape::new(5.0, 10.0, 6.0, 1.0, 0.0), Part::Feature),
        (Shape::new(4.0, 8.0, 8.0, 1.0, 0.0), Part::ScanLine),
    ],
    size: 16.0,
};

fn hex(Rgba(r, g, b, _): Rgba) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// The installed app icon (`vinoauthface.svg`): the ready state.
pub fn app_svg() -> String {
    let mut svg = String::from("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 64 64\">\n");
    let mut line = |element: String| {
        svg.push_str("  ");
        svg.push_str(&element);
        svg.push('\n');
    };
    line(LARGE.tile.svg_rect(&format!(r#"fill="{}""#, hex(TILE))));
    // SVG strokes straddle the path; inset by half to match stroke_rounded.
    line(LARGE.tile.inset(LARGE.edge / 2.0).svg_rect(&format!(
        r#"fill="none" stroke="{}" stroke-opacity="{:.2}" stroke-width="{}""#,
        hex(EDGE),
        f32::from(EDGE.3) / 255.0,
        LARGE.edge
    )));
    for &(shape, part) in LARGE.marks {
        if let Some(color) = paint(State::Ready, part) {
            line(shape.svg_rect(&format!(r#"fill="{}""#, hex(color))));
        }
    }
    svg.push_str("</svg>\n");
    svg
}

impl State {
    pub const ALL: [State; 3] = [State::Ready, State::Scanning, State::Attention];

    /// The themed icon name the tray asks for, as vinoWhisper's tray does:
    /// Plasma recolours a `-symbolic` icon to the panel's text colour, and the
    /// pixmaps below are only the fallback when the theme has none.
    pub fn icon_name(self) -> &'static str {
        match self {
            State::Ready => "vinoauthface-symbolic",
            State::Scanning => "vinoauthface-scanning-symbolic",
            State::Attention => "vinoauthface-attention-symbolic",
        }
    }
}

/// A monochrome tray icon for one state, in the KDE symbolic format: the
/// `ColorScheme-Text` class is what Plasma recolours. The viewfinder follows
/// the theme; the accent (green face, amber scan) stays fixed.
pub fn symbolic_svg(state: State) -> String {
    let (mut text, mut accent, mut dim) = (String::new(), String::new(), String::new());
    for &(shape, part) in SYMBOLIC.marks {
        let bucket = match (state, part) {
            (State::Ready, Part::Feature) | (State::Scanning, Part::Bracket | Part::ScanLine) => &mut accent,
            (State::Attention, _) => &mut dim,
            (_, Part::ScanLine) => continue,
            _ => &mut text,
        };
        bucket.push_str(&shape.svg_path());
    }
    let accent_fill = hex(if state == State::Ready { GREEN } else { AMBER });
    let mut svg = String::from(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 16 16\">\n  \
         <style id=\"current-color-scheme\" type=\"text/css\">.ColorScheme-Text { color: #232629; }</style>\n",
    );
    for (path, style) in [
        (text, "fill:currentColor".to_string()),
        (dim, "fill:currentColor;fill-opacity:0.5".to_string()),
        (accent, format!("fill:{accent_fill}")),
    ] {
        if !path.is_empty() {
            let class = if style.contains("currentColor") { r#" class="ColorScheme-Text""# } else { "" };
            svg.push_str(&format!("  <path{class} style=\"{style}\" d=\"{path}\"/>\n"));
        }
    }
    svg.push_str("</svg>\n");
    svg
}

/// Every size a tray might ask for, for one state.
pub fn tray_icons(state: State) -> Vec<ksni::Icon> {
    [16, 22, 24, 32, 48, 64].into_iter().map(|size| tray_icon(state, size)).collect()
}

fn tray_icon(state: State, size: u32) -> ksni::Icon {
    let mut pixels = vec![0u8; (size * size * 4) as usize];
    let mut canvas = Canvas::new(&mut pixels, size, size);
    let grid = if size < 32 { &SMALL } else { &LARGE };
    draw(&mut canvas, grid, state, size as f32);
    ksni::Icon {
        width: size as i32,
        height: size as i32,
        data: canvas.to_argb32_be(),
    }
}

fn draw(canvas: &mut Canvas, grid: &Grid, state: State, size: f32) {
    let unit = size / grid.size;
    let tile = grid.tile.rect(unit);
    canvas.fill_rounded(tile, grid.tile.r * unit, TILE);
    canvas.stroke_rounded(tile, grid.tile.r * unit, grid.edge * unit, EDGE);
    for &(shape, part) in grid.marks {
        if let Some(color) = paint(state, part) {
            canvas.fill_rounded(shape.rect(unit), shape.r * unit, color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const STATES: [State; 3] = State::ALL;

    #[test]
    fn every_size_is_a_complete_argb_image() {
        for state in STATES {
            for icon in tray_icons(state) {
                assert_eq!(icon.data.len(), (icon.width * icon.height * 4) as usize);
            }
        }
    }

    #[test]
    fn the_states_look_different_at_every_size() {
        for size in [16, 22, 64] {
            let [a, b, c] = STATES.map(|s| tray_icon(s, size).data);
            assert!(a != b && b != c && a != c, "{size}px states collide");
        }
    }

    #[test]
    fn the_corners_are_clear() {
        for size in [16, 64] {
            assert_eq!(tray_icon(State::Ready, size).data[0], 0, "{size}px corner is opaque");
        }
    }

    #[test]
    fn the_marks_sit_inside_the_tile() {
        for grid in [&LARGE, &SMALL, &SYMBOLIC] {
            let inner = grid.tile.inset(grid.edge);
            for (m, _) in grid.marks {
                assert!(
                    m.x > inner.x
                        && m.y > inner.y
                        && m.x + m.w < inner.x + inner.w
                        && m.y + m.h < inner.y + inner.h,
                    "{m:?} reaches the edge"
                );
                assert!(m.r * 2.0 <= m.w.min(m.h), "{m:?} is rounded past a pill");
            }
        }
    }

    #[test]
    fn the_face_and_scan_line_do_not_touch_the_brackets_or_each_other() {
        for grid in [&LARGE, &SMALL, &SYMBOLIC] {
            for (i, (a, part)) in grid.marks.iter().enumerate() {
                if *part == Part::Bracket {
                    continue;
                }
                for (j, (b, _)) in grid.marks.iter().enumerate() {
                    let apart = a.x + a.w < b.x || b.x + b.w < a.x || a.y + a.h < b.y || b.y + b.h < a.y;
                    assert!(i == j || apart, "{a:?} touches {b:?}");
                }
            }
        }
    }

    #[test]
    fn the_small_icon_lands_on_whole_pixels() {
        let shapes = std::iter::once(SMALL.tile).chain(SMALL.marks.iter().chain(SYMBOLIC.marks).map(|(s, _)| *s));
        for s in shapes {
            for edge in [s.x, s.y, s.w, s.h, SMALL.edge] {
                assert_eq!(edge.fract(), 0.0, "{s:?} is off the pixel grid");
            }
        }
    }

    #[test]
    fn the_checked_in_icons_are_the_ones_drawn_here() {
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("data");
        let bless = std::env::var_os("FACE_AUTH_BLESS_ICONS").is_some();
        let mut icons = vec![("vinoauthface.svg".to_string(), app_svg())];
        icons.extend(STATES.map(|s| (format!("{}.svg", s.icon_name()), symbolic_svg(s))));
        for (name, svg) in icons {
            let path = data.join(name);
            if bless {
                std::fs::write(&path, &svg).unwrap();
            }
            let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(
                on_disk == svg,
                "{} is stale; regenerate it with FACE_AUTH_BLESS_ICONS=1 cargo test -p face-auth-tray",
                path.display()
            );
        }
    }
}
