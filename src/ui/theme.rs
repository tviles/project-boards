use crate::model::OptionColor;
use ratatui::style::{Color, Modifier, Style};

/// Colours map GitHub's option colours onto the terminal's 16 ANSI colours, so the user's
/// terminal theme decides the actual shades. With NO_COLOR only modifiers are used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub color: bool,
}

impl Theme {
    /// NO_COLOR set to any non-empty value disables colour (https://no-color.org).
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Self {
        Self {
            color: get("NO_COLOR").is_none_or(|v| v.is_empty()),
        }
    }

    pub fn plain() -> Self {
        Self { color: false }
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
}
