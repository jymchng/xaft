//! Color theme for xaft TUI.

use ratatui::style::{Color, Modifier, Style};
use xaft_config::types::TuiTheme;

/// Resolved color palette for rendering.
#[derive(Debug, Clone)]
pub struct Theme {
    /// Background color for the main area.
    pub bg: Color,
    /// Primary foreground text.
    pub fg: Color,
    /// Dim / secondary text.
    pub dim: Color,
    /// Accent color (borders, highlights).
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
    /// Border color (unfocused).
    pub border: Color,
    /// Border color (focused).
    pub border_focused: Color,
    /// Status bar background.
    pub statusbar_bg: Color,
    /// Status bar foreground.
    pub statusbar_fg: Color,
    /// Approval modal background.
    pub modal_bg: Color,
    /// Approval modal text.
    pub modal_fg: Color,
}

impl Theme {
    pub fn dark() -> Self {
        // Muted, warm-neutral palette — less vibrant, easier on the eyes.
        // fg/bg desaturated; accent a soft slate-blue; success/error understated.
        Self {
            bg: Color::Rgb(16, 17, 19),
            fg: Color::Rgb(200, 198, 194),
            dim: Color::Rgb(90, 90, 92),
            accent: Color::Rgb(100, 149, 193),
            success: Color::Rgb(82, 168, 140),
            warning: Color::Rgb(190, 155, 75),
            error: Color::Rgb(185, 80, 75),
            tool: Color::Rgb(120, 170, 200),
            agent: Color::Rgb(170, 130, 200),
            border: Color::Rgb(48, 50, 54),
            border_focused: Color::Rgb(100, 149, 193),
            statusbar_bg: Color::Rgb(24, 25, 28),
            statusbar_fg: Color::Rgb(150, 150, 152),
            modal_bg: Color::Rgb(32, 34, 44),
            modal_fg: Color::Rgb(200, 198, 194),
        }
    }

    pub fn light() -> Self {
        Self {
            bg: Color::Rgb(250, 250, 250),
            fg: Color::Rgb(30, 30, 30),
            dim: Color::Rgb(120, 120, 120),
            accent: Color::Rgb(0, 112, 200),
            success: Color::Rgb(0, 150, 100),
            warning: Color::Rgb(160, 120, 0),
            error: Color::Rgb(200, 0, 0),
            tool: Color::Rgb(0, 100, 180),
            agent: Color::Rgb(140, 0, 180),
            border: Color::Rgb(200, 200, 200),
            border_focused: Color::Rgb(0, 112, 200),
            statusbar_bg: Color::Rgb(220, 220, 220),
            statusbar_fg: Color::Rgb(60, 60, 60),
            modal_bg: Color::Rgb(240, 240, 255),
            modal_fg: Color::Rgb(30, 30, 30),
        }
    }

    pub fn solarized() -> Self {
        Self {
            bg: Color::Rgb(0, 43, 54),
            fg: Color::Rgb(131, 148, 150),
            dim: Color::Rgb(88, 110, 117),
            accent: Color::Rgb(38, 139, 210),
            success: Color::Rgb(133, 153, 0),
            warning: Color::Rgb(181, 137, 0),
            error: Color::Rgb(220, 50, 47),
            tool: Color::Rgb(42, 161, 152),
            agent: Color::Rgb(108, 113, 196),
            border: Color::Rgb(7, 54, 66),
            border_focused: Color::Rgb(38, 139, 210),
            statusbar_bg: Color::Rgb(7, 54, 66),
            statusbar_fg: Color::Rgb(101, 123, 131),
            modal_bg: Color::Rgb(0, 43, 54),
            modal_fg: Color::Rgb(147, 161, 161),
        }
    }

    pub fn from_config(theme: &TuiTheme) -> Self {
        match theme {
            TuiTheme::Dark => Self::dark(),
            TuiTheme::Light => Self::light(),
            TuiTheme::Solarized => Self::solarized(),
        }
    }

    // ── Style helpers ─────────────────────────────────────────────────────────

    pub fn base(&self) -> Style {
        Style::default().fg(self.fg).bg(self.bg)
    }

    pub fn dim(&self) -> Style {
        Style::default().fg(self.dim)
    }

    pub fn accent(&self) -> Style {
        Style::default().fg(self.accent)
    }

    pub fn success(&self) -> Style {
        Style::default().fg(self.success)
    }

    pub fn warning(&self) -> Style {
        Style::default().fg(self.warning)
    }

    pub fn error(&self) -> Style {
        Style::default().fg(self.error)
    }

    pub fn tool(&self) -> Style {
        Style::default().fg(self.tool)
    }

    pub fn agent(&self) -> Style {
        Style::default().fg(self.agent).add_modifier(Modifier::BOLD)
    }

    pub fn border(&self) -> Style {
        Style::default().fg(self.border)
    }

    pub fn border_focused(&self) -> Style {
        Style::default().fg(self.border_focused)
    }

    pub fn statusbar(&self) -> Style {
        Style::default().fg(self.statusbar_fg).bg(self.statusbar_bg)
    }

    pub fn modal(&self) -> Style {
        Style::default().fg(self.modal_fg).bg(self.modal_bg)
    }

    pub fn bold(&self) -> Style {
        Style::default().fg(self.fg).add_modifier(Modifier::BOLD)
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
