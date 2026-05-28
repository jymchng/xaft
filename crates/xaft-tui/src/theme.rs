//! Color theme for xaft TUI.

use crossterm::style::Color;
use xaft_config::types::TuiTheme;

/// Resolved color palette for rendering.
#[derive(Debug, Clone)]
pub struct Theme {
    /// Background color (used for context, not drawn directly in transcript mode).
    pub bg: Color,
    /// Primary foreground text.
    pub fg: Color,
    /// Dim / secondary text.
    pub dim: Color,
    /// Accent color (prompt glyph, highlights).
    pub accent: Color,
    /// Success / approved indicator.
    pub success: Color,
    /// Warning / pending indicator.
    pub warning: Color,
    /// Error / rejected indicator.
    pub error: Color,
    /// Tool name color.
    pub tool: Color,
    /// Agent name color.
    pub agent: Color,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            bg: Color::Rgb {
                r: 16,
                g: 17,
                b: 19,
            },
            fg: Color::Rgb {
                r: 200,
                g: 198,
                b: 194,
            },
            dim: Color::Rgb {
                r: 90,
                g: 90,
                b: 92,
            },
            accent: Color::Rgb {
                r: 100,
                g: 149,
                b: 193,
            },
            success: Color::Rgb {
                r: 82,
                g: 168,
                b: 140,
            },
            warning: Color::Rgb {
                r: 190,
                g: 155,
                b: 75,
            },
            error: Color::Rgb {
                r: 185,
                g: 80,
                b: 75,
            },
            tool: Color::Rgb {
                r: 120,
                g: 170,
                b: 200,
            },
            agent: Color::Rgb {
                r: 170,
                g: 130,
                b: 200,
            },
        }
    }

    pub fn light() -> Self {
        Self {
            bg: Color::Rgb {
                r: 250,
                g: 250,
                b: 250,
            },
            fg: Color::Rgb {
                r: 30,
                g: 30,
                b: 30,
            },
            dim: Color::Rgb {
                r: 120,
                g: 120,
                b: 120,
            },
            accent: Color::Rgb {
                r: 0,
                g: 112,
                b: 200,
            },
            success: Color::Rgb {
                r: 0,
                g: 150,
                b: 100,
            },
            warning: Color::Rgb {
                r: 160,
                g: 120,
                b: 0,
            },
            error: Color::Rgb { r: 200, g: 0, b: 0 },
            tool: Color::Rgb {
                r: 0,
                g: 100,
                b: 180,
            },
            agent: Color::Rgb {
                r: 140,
                g: 0,
                b: 180,
            },
        }
    }

    pub fn solarized() -> Self {
        Self {
            bg: Color::Rgb { r: 0, g: 43, b: 54 },
            fg: Color::Rgb {
                r: 131,
                g: 148,
                b: 150,
            },
            dim: Color::Rgb {
                r: 88,
                g: 110,
                b: 117,
            },
            accent: Color::Rgb {
                r: 38,
                g: 139,
                b: 210,
            },
            success: Color::Rgb {
                r: 133,
                g: 153,
                b: 0,
            },
            warning: Color::Rgb {
                r: 181,
                g: 137,
                b: 0,
            },
            error: Color::Rgb {
                r: 220,
                g: 50,
                b: 47,
            },
            tool: Color::Rgb {
                r: 42,
                g: 161,
                b: 152,
            },
            agent: Color::Rgb {
                r: 108,
                g: 113,
                b: 196,
            },
        }
    }

    pub fn from_config(theme: &TuiTheme) -> Self {
        match theme {
            TuiTheme::Dark => Self::dark(),
            TuiTheme::Light => Self::light(),
            TuiTheme::Solarized => Self::solarized(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_theme_has_distinct_colors() {
        let t = Theme::dark();
        assert_ne!(t.success, t.error);
        assert_ne!(t.bg, t.fg);
        assert_ne!(t.accent, t.bg);
    }

    #[test]
    fn all_themes_construct() {
        let _ = Theme::dark();
        let _ = Theme::light();
        let _ = Theme::solarized();
    }

    #[test]
    fn from_config_maps_correctly() {
        let _ = Theme::from_config(&TuiTheme::Dark);
        let _ = Theme::from_config(&TuiTheme::Light);
        let _ = Theme::from_config(&TuiTheme::Solarized);
    }
}
