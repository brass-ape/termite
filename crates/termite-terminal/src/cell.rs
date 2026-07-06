// SPDX-License-Identifier: MIT
//! A single character cell in the terminal grid, and its display attributes.

/// A colour used for a cell's foreground or background.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TermColor {
    /// The terminal's configured default colour.
    #[default]
    Default,
    /// One of the 256 indexed palette colours.
    Indexed(u8),
    /// A 24-bit true colour.
    Rgb(u8, u8, u8),
}

/// Text display attributes controlled by SGR escape sequences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attrs {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
    pub strikethrough: bool,
}

/// A single character cell: its glyph, colours, and attributes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell {
    pub ch: char,
    pub fg: TermColor,
    pub bg: TermColor,
    pub attrs: Attrs,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: TermColor::Default,
            bg: TermColor::Default,
            attrs: Attrs::default(),
        }
    }
}
