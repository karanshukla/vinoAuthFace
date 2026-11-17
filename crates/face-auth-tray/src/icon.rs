//! The tray and app icon, in the vino apps' style: a dark rounded tile with a
//! faint edge and a few heavy marks. Here the marks are the IR camera module
//! itself: a grey outline holding a white lens ring and, beside it, the
//! emitter dot that lights up for a scan, as the one green accent.
//! Drawn in code so the tray needs no image decoder, and the checked-in SVG
//! is generated from the same shapes.

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
    /// The camera module, drawn as an outline `Grid::ring` wide.
    Module,
    /// A ring `Grid::lens_ring` wide, so it reads as a lens and not as a
    /// switch's knob.
    Lens,
    /// The IR emitter: green when ready, amber while scanning, dark until
    /// setup is finished.
    Emitter,
}

// The vino apps' palette (vinoWhisper's gui/src/paint.rs).
const TILE: Rgba = Rgba(14, 14, 16, 255);
const EDGE: Rgba = Rgba(255, 255, 255, 46);
const WHITE: Rgba = Rgba(250, 250, 250, 255);
const GREY: Rgba = Rgba(160, 164, 172, 255);
const GREEN: Rgba = Rgba(86, 204, 120, 255);
/// The grey at 0.6 over the tile; the other logos' 0.5 falls just short of
/// 3:1 on it (pinned by `tests::the_pixmap_marks_stand_out_on_any_panel`).
const FRAME: Rgba = Rgba(102, 104, 110, 255);
const AMBER: Rgba = Rgba(245, 184, 66, 255);

/// The symbolic icon has no tile, so its accents are darker shades that keep
/// 3:1 on light and dark panels (pinned by
/// `tests::the_symbolic_icons_show_on_light_and_dark_panels`).
const TRAY_GREEN: Rgba = Rgba(40, 150, 80, 255);
const TRAY_AMBER: Rgba = Rgba(184, 115, 0, 255);

fn paint(state: State, part: Part) -> Option<Rgba> {
    match (state, part) {
        (State::Ready, Part::Module) => Some(FRAME),
        (State::Ready | State::Scanning, Part::Lens) => Some(WHITE),
        (State::Ready, Part::Emitter) => Some(GREEN),
        (State::Scanning, Part::Module | Part::Emitter) => Some(AMBER),
        (State::Attention, Part::Module | Part::Lens) => Some(GREY),
        (State::Attention, Part::Emitter) => None,
    }
}

struct Grid {
    tile: Shape,
    edge: f32,
    /// How wide the module's outline is drawn.
    ring: f32,
    lens_ring: f32,
    marks: &'static [(Shape, Part)],
    size: f32,
}

impl Grid {
    /// The outline width a part is drawn with, or `None` for a filled one.
    fn ring_of(&self, part: Part) -> Option<f32> {
        match part {
            Part::Module => Some(self.ring),
            Part::Lens => Some(self.lens_ring),
            Part::Emitter => None,
        }
    }
}

/// 64-unit design, for the app icon and the larger tray sizes. The tile and
/// the three tones (a grey frame, a white mark, a green accent) are the other
/// vino apps'.
const LARGE: Grid = Grid {
    tile: Shape::new(4.0, 4.0, 56.0, 56.0, 14.0),
    edge: 2.0,
    ring: 5.0,
    lens_ring: 4.0,
    marks: &[
        (Shape::new(10.0, 18.0, 44.0, 28.0, 9.0), Part::Module),
        (Shape::new(17.0, 25.0, 14.0, 14.0, 7.0), Part::Lens),
        (Shape::new(36.5, 27.5, 9.0, 9.0, 4.5), Part::Emitter),
    ],
    size: 64.0,
};

/// 16-unit design on whole pixels, so the 16 px tray icon stays sharp. There
/// is no room for the module's outline at this size: the tile stands in for
/// it, around the lens and the emitter.
const SMALL: Grid = Grid {
    tile: Shape::new(1.0, 1.0, 14.0, 14.0, 3.0),
    edge: 1.0,
    ring: 0.0,
    lens_ring: 2.0,
    marks: &[
        (Shape::new(3.0, 5.0, 6.0, 6.0, 3.0), Part::Lens),
        (Shape::new(11.0, 7.0, 2.0, 2.0, 1.0), Part::Emitter),
    ],
    size: 16.0,
};

/// The vino trays' shared frame (vinoWhisper's and vinoWatch's symbolic
/// icons): a 14x12 rounded tile drawn as a 1-unit outline.
const TRAY_FRAME: &str = "M3 2h10a2 2 0 0 1 2 2v8a2 2 0 0 1 -2 2h-10a2 2 0 0 1 -2 -2v-8a2 2 0 0 1 2 -2zM3 3h10a1 1 0 0 1 1 1v8a1 1 0 0 1 -1 1h-10a1 1 0 0 1 -1 -1v-8a1 1 0 0 1 1 -1z";
/// The camera module inside it: the lens, and the emitter dot beside it.
const TRAY_LENS: &str = "M6 5a3 3 0 1 1 0 6a3 3 0 1 1 0 -6zM6 6.75a1.25 1.25 0 1 0 0 2.5a1.25 1.25 0 1 0 0 -2.5z";
const TRAY_EMITTER: &str = "M11.5 6.5a1.5 1.5 0 1 1 0 3a1.5 1.5 0 1 1 0 -3z";

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
            match LARGE.ring_of(part) {
                Some(width) => line(shape.inset(width / 2.0).svg_rect(&format!(
                    r#"fill="none" stroke="{}" stroke-width="{width}""#,
                    hex(color)
                ))),
                None => line(shape.svg_rect(&format!(r#"fill="{}""#, hex(color)))),
            }
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

/// A tray icon for one state, in the vino trays' symbolic style and the KDE
/// symbolic format (`ColorScheme-Text` is what Plasma recolours): the shared
/// frame and the lens ring in the theme's colour, the emitter as the one accent,
/// green when ready and amber while a scan runs, as vinoWatch's accent turns
/// amber. Until setup is finished the lens is dimmed and the emitter is off;
/// at 0.6, not 0.5: half of Breeze Light's text colour falls just short of
/// 3:1 on its panel.
pub fn symbolic_svg(state: State) -> String {
    let text = r#"class="ColorScheme-Text" style="fill:currentColor""#;
    let dim = r#"class="ColorScheme-Text" style="fill:currentColor;fill-opacity:0.6""#;
    let accent = |color: Rgba| format!(r#"style="fill:{}""#, hex(color));
    let lens = |paint: &str| format!(r#"{paint} fill-rule="evenodd""#);
    let mut paths = vec![(format!(r#"{text} fill-rule="evenodd""#), TRAY_FRAME)];
    match state {
        State::Ready => paths.extend([(lens(text), TRAY_LENS), (accent(TRAY_GREEN), TRAY_EMITTER)]),
        State::Scanning => paths.extend([(lens(text), TRAY_LENS), (accent(TRAY_AMBER), TRAY_EMITTER)]),
        State::Attention => paths.push((lens(dim), TRAY_LENS)),
    }
    let mut svg = String::from(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 16 16\">\n  \
         <style id=\"current-color-scheme\" type=\"text/css\">.ColorScheme-Text { color: #232629; }</style>\n",
    );
    for (paint, d) in paths {
        svg.push_str(&format!("  <path {paint} d=\"{d}\"/>\n"));
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
            match grid.ring_of(part) {
                Some(width) => canvas.stroke_rounded(shape.rect(unit), shape.r * unit, width * unit, color),
                None => canvas.fill_rounded(shape.rect(unit), shape.r * unit, color),
            }
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
        for grid in [&LARGE, &SMALL] {
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
    fn the_lens_and_emitter_sit_inside_the_module_and_apart() {
        for grid in [&LARGE, &SMALL] {
            let inside: Vec<Shape> = grid.marks.iter().filter(|(_, p)| *p != Part::Module).map(|(s, _)| *s).collect();
            for (module, _) in grid.marks.iter().filter(|(_, p)| *p == Part::Module) {
                let hole = module.inset(grid.ring);
                for m in &inside {
                    assert!(
                        m.x > hole.x && m.y > hole.y && m.x + m.w < hole.x + hole.w && m.y + m.h < hole.y + hole.h,
                        "{m:?} touches the module's outline"
                    );
                }
            }
            let [lens, emitter] = inside[..] else { panic!("expected a lens and an emitter") };
            assert!(lens.x + lens.w < emitter.x, "the lens touches the emitter");
        }
    }

    /// The emitter is what lights up: off until setup is finished, and a
    /// different colour while a scan runs.
    #[test]
    fn the_emitter_shows_the_state() {
        assert_eq!(paint(State::Attention, Part::Emitter), None);
        let (ready, scanning) = (paint(State::Ready, Part::Emitter), paint(State::Scanning, Part::Emitter));
        assert!(ready.is_some() && scanning.is_some() && ready != scanning);
    }

    #[test]
    fn the_small_icon_lands_on_whole_pixels() {
        let shapes = std::iter::once(SMALL.tile).chain(SMALL.marks.iter().map(|(s, _)| *s));
        for s in shapes {
            for edge in [s.x, s.y, s.w, s.h, SMALL.edge] {
                assert_eq!(edge.fract(), 0.0, "{s:?} is off the pixel grid");
            }
        }
    }

    fn luminance(Rgba(r, g, b, _): Rgba) -> f32 {
        let channel = |c: u8| {
            let c = f32::from(c) / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
    }

    fn contrast(a: Rgba, b: Rgba) -> f32 {
        let (a, b) = (luminance(a) + 0.05, luminance(b) + 0.05);
        a.max(b) / a.min(b)
    }

    /// `top` at `alpha` over `under`, blended in sRGB as SVG renderers do.
    fn over(top: Rgba, alpha: f32, under: Rgba) -> Rgba {
        let mix = |t: u8, u: u8| (f32::from(t) * alpha + f32::from(u) * (1.0 - alpha)).round() as u8;
        Rgba(mix(top.0, under.0), mix(top.1, under.1), mix(top.2, under.2), 255)
    }

    /// Panel backgrounds, each with the text colour the theme recolours a
    /// symbolic icon to: Breeze Light, plain white, Breeze Dark (Plasma 6
    /// and 5), GNOME's black top bar, and waybar's default.
    const PANELS: [(Rgba, Rgba); 6] = [
        (Rgba(239, 240, 241, 255), Rgba(35, 38, 41, 255)),
        (Rgba(255, 255, 255, 255), Rgba(0, 0, 0, 255)),
        (Rgba(32, 35, 38, 255), Rgba(252, 252, 252, 255)),
        (Rgba(49, 54, 59, 255), Rgba(252, 252, 252, 255)),
        (Rgba(0, 0, 0, 255), Rgba(255, 255, 255, 255)),
        (Rgba(43, 48, 59, 255), Rgba(255, 255, 255, 255)),
    ];

    /// Each mark of each pixmap, at every size, reaches 3:1 against the tile
    /// it sits on (WCAG 1.4.11). The tile is opaque and surrounds every mark,
    /// so the panel never touches them and this holds on any panel.
    #[test]
    fn the_pixmap_marks_stand_out_on_any_panel() {
        for state in STATES {
            for icon in tray_icons(state) {
                let size = icon.width as u32;
                let grid = if size < 32 { &SMALL } else { &LARGE };
                let unit = size as f32 / grid.size;
                let pixel = |x: u32, y: u32| {
                    let i = ((y * size + x) * 4) as usize;
                    let [a, r, g, b] = icon.data[i..i + 4] else { unreachable!() };
                    Rgba(r, g, b, a)
                };
                for &(shape, part) in grid.marks {
                    if paint(state, part).is_none() {
                        continue;
                    }
                    // The mark's best pixel: at sizes off its grid, a thin
                    // mark is only part-covered everywhere.
                    let r = shape.rect(unit);
                    let best = (r.y0 as u32..r.y1.ceil() as u32)
                        .flat_map(|y| (r.x0 as u32..r.x1.ceil() as u32).map(move |x| (x, y)))
                        .map(|(x, y)| pixel(x, y))
                        .max_by(|a, b| contrast(*a, TILE).total_cmp(&contrast(*b, TILE)))
                        .unwrap();
                    assert_eq!(best.3, 255, "{state:?} {size}px {part:?} is see-through");
                    let on_tile = contrast(best, TILE);
                    assert!(on_tile >= 3.0, "{state:?} {size}px {part:?} {best:?} is {on_tile:.2}:1 on the tile");
                }
            }
        }
        for (panel, _) in PANELS.iter().filter(|(p, _)| luminance(*p) > 0.5) {
            assert!(contrast(TILE, *panel) >= 3.0, "the tile vanishes on {panel:?}");
        }
    }

    /// The symbolic icons have no tile: every path, as the theme recolours it,
    /// must clear 3:1 against the panel. That includes the attention state's
    /// dimmed face, not just the fixed accents.
    #[test]
    fn the_symbolic_icons_show_on_light_and_dark_panels() {
        for state in STATES {
            let svg = symbolic_svg(state);
            let styles: Vec<&str> = svg.split("style=\"").skip(1).map(|s| s.split('"').next().unwrap()).collect();
            assert!(!styles.is_empty(), "{state:?} draws nothing");
            for (panel, text) in PANELS {
                for style in &styles {
                    let fill = if style.contains("currentColor") {
                        let alpha = style.split("fill-opacity:").nth(1).map_or(1.0, |a| a.parse().unwrap());
                        over(text, alpha, panel)
                    } else {
                        let hex = style.split(":#").nth(1).unwrap();
                        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap();
                        Rgba(byte(0), byte(2), byte(4), 255)
                    };
                    let c = contrast(fill, panel);
                    assert!(c >= 3.0, "{state:?} {style} is {c:.2}:1 on {panel:?}");
                }
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
