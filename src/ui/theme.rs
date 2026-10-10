use crate::model::OptionColor;
use ratatui::style::{Color, Modifier, Style};

/// Colours map GitHub's option colours onto the terminal's 16 ANSI colours, so the user's
/// terminal theme decides the actual shades. With NO_COLOR only modifiers are used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub color: bool,
    /// The terminal can show 24-bit colour (`COLORTERM=truecolor` or `24bit`).
    pub truecolor: bool,
}

impl Theme {
    /// NO_COLOR set to any non-empty value disables colour (https://no-color.org).
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Self {
        Self {
            color: get("NO_COLOR").is_none_or(|v| v.is_empty()),
            truecolor: get("COLORTERM").is_some_and(|v| {
                v.eq_ignore_ascii_case("truecolor") || v.eq_ignore_ascii_case("24bit")
            }),
        }
    }

    pub fn plain() -> Self {
        Self {
            color: false,
            truecolor: false,
        }
    }

    fn fg(&self, color: Color) -> Style {
        if self.color {
            Style::default().fg(color)
        } else {
            Style::default()
        }
    }

    pub fn option(&self, c: OptionColor) -> Style {
        self.fg(match c {
            OptionColor::Gray => Color::Gray,
            OptionColor::Blue => Color::Blue,
            OptionColor::Green => Color::Green,
            OptionColor::Yellow => Color::Yellow,
            OptionColor::Orange => Color::LightRed,
            OptionColor::Red => Color::Red,
            OptionColor::Pink => Color::LightMagenta,
            OptionColor::Purple => Color::Magenta,
            OptionColor::Unknown => Color::Reset,
        })
    }

    /// A label pill: GitHub's colour as the background (exact with truecolor, otherwise the
    /// nearest xterm-256 colour) with black or white text, whichever reads better. Plain
    /// without colour or when `hex` is not a 3- or 6-digit hex colour.
    pub fn label(&self, hex: &str) -> Style {
        let Some(rgb) = parse_hex(hex).filter(|_| self.color) else {
            return Style::default();
        };
        // The text colour follows the background actually shown, not the requested one.
        let (bg, shown) = if self.truecolor {
            (Color::Rgb(rgb.0, rgb.1, rgb.2), rgb)
        } else {
            let index = nearest_256(rgb);
            (Color::Indexed(index), index_rgb(index))
        };
        Style::default().bg(bg).fg(contrast_fg(shown))
    }

    pub fn selected(&self) -> Style {
        Style::default().add_modifier(Modifier::REVERSED)
    }
    pub fn dim(&self) -> Style {
        Style::default().add_modifier(Modifier::DIM)
    }
    pub fn bold(&self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }
    pub fn heading(&self) -> Style {
        self.fg(Color::Cyan).add_modifier(Modifier::BOLD)
    }
    pub fn error(&self) -> Style {
        self.fg(Color::Red).add_modifier(Modifier::BOLD)
    }
    pub fn accent(&self) -> Style {
        self.fg(Color::Cyan)
    }
    pub fn code(&self) -> Style {
        if self.color {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().add_modifier(Modifier::ITALIC)
        }
    }
    pub fn link(&self) -> Style {
        self.fg(Color::Blue).add_modifier(Modifier::UNDERLINED)
    }
}

/// `rrggbb` or `rgb` (optionally after a `#`).
fn parse_hex(hex: &str) -> Option<(u8, u8, u8)> {
    let h = hex.strip_prefix('#').unwrap_or(hex);
    if !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let digit = |i: usize| u8::from_str_radix(&h[i..=i], 16).ok();
    let pair = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    match h.len() {
        6 => Some((pair(0)?, pair(2)?, pair(4)?)),
        3 => Some((digit(0)? * 17, digit(1)? * 17, digit(2)? * 17)),
        _ => None,
    }
}

fn linear(channel: u8) -> f64 {
    let c = f64::from(channel) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Black or white text, whichever contrasts more with `rgb`. The crossover of the two
/// contrast ratios is a luminance of about 0.179; 0.18 keeps GitHub's own red (`d73a4a`,
/// luminance 0.1797) on white text.
fn contrast_fg((r, g, b): (u8, u8, u8)) -> Color {
    let luminance = 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
    if luminance > 0.18 {
        Color::Black
    } else {
        Color::White
    }
}

const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// The RGB of an xterm-256 colour in the cube (16..=231) or the grey ramp (232..=255).
fn index_rgb(index: u8) -> (u8, u8, u8) {
    if index >= 232 {
        let v = 8 + 10 * (index - 232);
        return (v, v, v);
    }
    let i = usize::from(index.saturating_sub(16));
    (
        CUBE_LEVELS[(i / 36) % 6],
        CUBE_LEVELS[(i / 6) % 6],
        CUBE_LEVELS[i % 6],
    )
}

/// The xterm-256 colour (6x6x6 cube or grey ramp; not the 16 theme-dependent ones)
/// nearest to `rgb` by squared RGB distance.
fn nearest_256((r, g, b): (u8, u8, u8)) -> u8 {
    let distance = |(cr, cg, cb): (u8, u8, u8)| {
        let d = |a: u8, b: u8| (i32::from(a) - i32::from(b)).pow(2);
        d(r, cr) + d(g, cg) + d(b, cb)
    };
    let cube = (0..216u8).map(|i| (distance(index_rgb(16 + i)), 16 + i));
    let grey = (232..=255u8).map(|i| (distance(index_rgb(i)), i));
    cube.chain(grey)
        .min_by_key(|(d, _)| *d)
        .map_or(16, |(_, index)| index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_color_disables_foregrounds_but_keeps_modifiers() {
        let t = Theme::from_env(|k| (k == "NO_COLOR").then(|| "1".to_string()));
        assert!(!t.color);
        assert_eq!(t.option(OptionColor::Red).fg, None);
        assert!(t.error().add_modifier.contains(Modifier::BOLD));
        assert!(t.selected().add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn color_maps_github_colors_to_ansi() {
        let t = Theme::from_env(|_| None);
        assert_eq!(t.option(OptionColor::Green).fg, Some(Color::Green));
        assert_eq!(t.option(OptionColor::Orange).fg, Some(Color::LightRed));
    }

    #[test]
    fn hex_parses_six_and_three_digits_and_rejects_the_rest() {
        assert_eq!(parse_hex("d73a4a"), Some((0xd7, 0x3a, 0x4a)));
        assert_eq!(parse_hex("fff"), Some((255, 255, 255)));
        assert_eq!(parse_hex("#0af"), Some((0, 0xaa, 0xff)));
        for bad in ["", "ff", "ffff", "d73a4", "zzzzzz", "ggg", "d73a4a0", "é1é"] {
            assert_eq!(parse_hex(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn text_is_black_on_light_and_white_on_dark() {
        let fg = |hex: &str| contrast_fg(parse_hex(hex).unwrap());
        assert_eq!(fg("ffffff"), Color::Black);
        assert_eq!(fg("fef2c0"), Color::Black);
        assert_eq!(fg("d73a4a"), Color::White);
        assert_eq!(fg("000000"), Color::White);
    }

    #[test]
    fn nearest_256_uses_the_cube_and_the_grey_ramp() {
        assert_eq!(nearest_256((255, 0, 0)), 196);
        assert_eq!(nearest_256((0, 0, 0)), 16);
        assert_eq!(nearest_256((255, 255, 255)), 231);
        assert_eq!(nearest_256((128, 128, 128)), 244);
        assert!((232..=255).contains(&nearest_256((100, 101, 100))));
    }

    #[test]
    fn index_rgb_matches_the_cube_and_grey_ramp() {
        assert_eq!(index_rgb(16), (0, 0, 0));
        assert_eq!(index_rgb(196), (255, 0, 0));
        assert_eq!(index_rgb(231), (255, 255, 255));
        assert_eq!(index_rgb(232), (8, 8, 8));
        assert_eq!(index_rgb(255), (238, 238, 238));
    }

    #[test]
    fn in_256_colours_the_text_follows_the_displayed_background() {
        // a05050 is dark enough for white text, but quantises to index 131 (af5f5f),
        // which is light enough for black text.
        let style = Theme::from_env(|_| None).label("a05050");
        assert_eq!(style.bg, Some(Color::Indexed(131)));
        assert_eq!(style.fg, Some(Color::Black));
        assert_eq!(contrast_fg(parse_hex("a05050").unwrap()), Color::White);
        let tc = Theme::from_env(|k| (k == "COLORTERM").then(|| "truecolor".to_string()));
        assert_eq!(tc.label("a05050").fg, Some(Color::White));
    }

    #[test]
    fn label_style_follows_colour_support() {
        let env = |pairs: &'static [(&str, &str)]| {
            Theme::from_env(move |k| {
                pairs
                    .iter()
                    .find(|(name, _)| *name == k)
                    .map(|(_, v)| v.to_string())
            })
        };
        let tc = env(&[("COLORTERM", "TrueColor")]).label("d73a4a");
        assert_eq!(tc.bg, Some(Color::Rgb(0xd7, 0x3a, 0x4a)));
        assert_eq!(tc.fg, Some(Color::White));
        assert!(env(&[("COLORTERM", "24bit")]).truecolor);
        let indexed = env(&[]).label("ff0000");
        assert_eq!(indexed.bg, Some(Color::Indexed(196)));
        assert_eq!(env(&[]).label("nope"), Style::default());
        let no_color = env(&[("NO_COLOR", "1"), ("COLORTERM", "truecolor")]);
        assert_eq!(no_color.label("d73a4a"), Style::default());
        assert_eq!(Theme::plain().label("d73a4a"), Style::default());
    }
}
