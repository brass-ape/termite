// SPDX-License-Identifier: MIT
//! Termite's visual theme — colour palette, typography, and spacing.

use iced::Theme;

/// Returns the base Iced theme for Termite.
///
/// Dark mode first. Light mode is a future milestone.
pub fn termite_theme() -> Theme {
    Theme::Dark
}

/// Marker type used when the theme needs to be passed as a value.
pub struct TermiteTheme;

impl TermiteTheme {
    pub fn iced() -> Theme {
        termite_theme()
    }
}

// ── Colour palette (design tokens) ───────────────────────────────────────────
// All colours referenced here are the intended final palette.
// Individual widget implementations reference these constants rather than
// hard-coding hex values.

pub mod colours {
    use iced::Color;

    /// Primary background — deepest surface.
    pub const BACKGROUND:      Color = Color::from_rgb(0.094, 0.094, 0.098); // #181819

    /// Secondary surface — panels, sidebars.
    pub const SURFACE:         Color = Color::from_rgb(0.122, 0.122, 0.129); // #1F1F21

    /// Elevated surface — cards, modals.
    pub const SURFACE_RAISED:  Color = Color::from_rgb(0.157, 0.157, 0.165); // #282829

    /// Primary text.
    pub const TEXT:            Color = Color::from_rgb(0.922, 0.918, 0.906); // #EBEAE7

    /// Secondary / muted text.
    pub const TEXT_MUTED:      Color = Color::from_rgb(0.565, 0.561, 0.549); // #908F8C

    /// Accent — used for focused borders, highlights, active tabs.
    pub const ACCENT:          Color = Color::from_rgb(0.251, 0.557, 0.996); // #408EFE

    /// Destructive — errors, host key warnings.
    pub const DESTRUCTIVE:     Color = Color::from_rgb(0.996, 0.329, 0.329); // #FE5454

    /// Success — connected indicator.
    pub const SUCCESS:         Color = Color::from_rgb(0.259, 0.792, 0.490); // #42CA7D
}
