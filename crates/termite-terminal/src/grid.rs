// SPDX-License-Identifier: MIT
//! The terminal grid: a 2D buffer of cells, cursor state, scrollback, and the
//! primary/alternate screen split.

use std::collections::VecDeque;

use crate::cell::{Attrs, Cell, TermColor};

/// How much of the line/display to erase for CSI `K`/`J`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EraseMode {
    /// From the cursor to the end.
    ToEnd,
    /// From the start to the cursor.
    ToStart,
    /// The entire line or display.
    All,
}

impl EraseMode {
    /// Maps the numeric parameter used by CSI `J`/`K` to an [`EraseMode`].
    pub fn from_param(n: u16) -> Self {
        match n {
            1 => EraseMode::ToStart,
            2 | 3 => EraseMode::All,
            _ => EraseMode::ToEnd,
        }
    }
}

/// A fixed-size 2D buffer of [`Cell`]s, plus the cursor and screen-mode state
/// needed to interpret a VT byte stream.
#[derive(Debug, Clone)]
pub struct TerminalGrid {
    rows: usize,
    cols: usize,

    primary: Vec<Vec<Cell>>,
    alternate: Vec<Vec<Cell>>,
    on_alt_screen: bool,

    scrollback: VecDeque<Vec<Cell>>,
    scrollback_limit: usize,

    cursor_row: usize,
    cursor_col: usize,
    saved_cursor: Option<(usize, usize)>,

    pending_fg: TermColor,
    pending_bg: TermColor,
    pending_attrs: Attrs,

    title: String,
}

impl TerminalGrid {
    pub fn new(rows: usize, cols: usize) -> Self {
        Self {
            rows,
            cols,
            primary: vec![vec![Cell::default(); cols]; rows],
            alternate: vec![vec![Cell::default(); cols]; rows],
            on_alt_screen: false,
            scrollback: VecDeque::new(),
            scrollback_limit: 10_000,
            cursor_row: 0,
            cursor_col: 0,
            saved_cursor: None,
            pending_fg: TermColor::Default,
            pending_bg: TermColor::Default,
            pending_attrs: Attrs::default(),
            title: String::new(),
        }
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn set_title(&mut self, title: impl Into<String>) {
        self.title = title.into();
    }

    /// Renders the visible screen as plain text rows, one `String` per row.
    pub fn visible_rows(&self) -> Vec<String> {
        self.screen()
            .iter()
            .map(|row| row.iter().map(|cell| cell.ch).collect())
            .collect()
    }

    pub fn cursor_position(&self) -> (usize, usize) {
        (self.cursor_row, self.cursor_col)
    }

    fn screen(&self) -> &Vec<Vec<Cell>> {
        if self.on_alt_screen {
            &self.alternate
        } else {
            &self.primary
        }
    }

    fn screen_mut(&mut self) -> &mut Vec<Vec<Cell>> {
        if self.on_alt_screen {
            &mut self.alternate
        } else {
            &mut self.primary
        }
    }

    pub fn resize(&mut self, rows: usize, cols: usize) {
        for screen in [&mut self.primary, &mut self.alternate] {
            screen.resize(rows, vec![Cell::default(); cols]);
            for row in screen.iter_mut() {
                row.resize(cols, Cell::default());
            }
        }
        self.rows = rows;
        self.cols = cols;
        self.cursor_row = self.cursor_row.min(rows.saturating_sub(1));
        self.cursor_col = self.cursor_col.min(cols.saturating_sub(1));
    }

    // ── Writing ──────────────────────────────────────────────────────────

    pub fn put_char(&mut self, ch: char) {
        if self.cursor_col >= self.cols {
            self.carriage_return();
            self.linefeed();
        }

        let (fg, bg, attrs) = (self.pending_fg, self.pending_bg, self.pending_attrs);
        let row = self.cursor_row;
        let col = self.cursor_col;
        let on_alt = self.on_alt_screen;
        let screen = if on_alt {
            &mut self.alternate
        } else {
            &mut self.primary
        };
        screen[row][col] = Cell { ch, fg, bg, attrs };
        self.cursor_col += 1;
    }

    pub fn linefeed(&mut self) {
        if self.cursor_row + 1 >= self.rows {
            self.scroll_up(1);
        } else {
            self.cursor_row += 1;
        }
    }

    pub fn carriage_return(&mut self) {
        self.cursor_col = 0;
    }

    pub fn backspace(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        }
    }

    fn scroll_up(&mut self, n: usize) {
        let on_alt = self.on_alt_screen;
        for _ in 0..n {
            let screen = if on_alt {
                &mut self.alternate
            } else {
                &mut self.primary
            };
            let removed = screen.remove(0);
            if !on_alt {
                if self.scrollback.len() >= self.scrollback_limit {
                    self.scrollback.pop_front();
                }
                self.scrollback.push_back(removed);
            }
            let screen = if on_alt {
                &mut self.alternate
            } else {
                &mut self.primary
            };
            screen.push(vec![Cell::default(); self.cols]);
        }
    }

    // ── Cursor movement ──────────────────────────────────────────────────

    pub fn cursor_up(&mut self, n: usize) {
        self.cursor_row = self.cursor_row.saturating_sub(n);
    }

    pub fn cursor_down(&mut self, n: usize) {
        self.cursor_row = (self.cursor_row + n).min(self.rows.saturating_sub(1));
    }

    pub fn cursor_forward(&mut self, n: usize) {
        self.cursor_col = (self.cursor_col + n).min(self.cols.saturating_sub(1));
    }

    pub fn cursor_back(&mut self, n: usize) {
        self.cursor_col = self.cursor_col.saturating_sub(n);
    }

    /// CSI `H`/`f` — 1-indexed row/column cursor position.
    pub fn set_cursor_position(&mut self, row: usize, col: usize) {
        self.cursor_row = row.saturating_sub(1).min(self.rows.saturating_sub(1));
        self.cursor_col = col.saturating_sub(1).min(self.cols.saturating_sub(1));
    }

    pub fn save_cursor(&mut self) {
        self.saved_cursor = Some((self.cursor_row, self.cursor_col));
    }

    pub fn restore_cursor(&mut self) {
        if let Some((row, col)) = self.saved_cursor {
            self.cursor_row = row;
            self.cursor_col = col;
        }
    }

    // ── Erasing ──────────────────────────────────────────────────────────

    pub fn erase_line(&mut self, mode: EraseMode) {
        let (row, col, cols) = (self.cursor_row, self.cursor_col, self.cols);
        let line = &mut self.screen_mut()[row];
        match mode {
            EraseMode::ToEnd => line[col..].fill(Cell::default()),
            EraseMode::ToStart => line[..=col.min(cols - 1)].fill(Cell::default()),
            EraseMode::All => line.fill(Cell::default()),
        }
    }

    pub fn erase_display(&mut self, mode: EraseMode) {
        match mode {
            EraseMode::All => {
                let (rows, cols) = (self.rows, self.cols);
                self.screen_mut().fill(vec![Cell::default(); cols]);
                let _ = rows;
            }
            EraseMode::ToEnd => {
                let row = self.cursor_row;
                self.erase_line(EraseMode::ToEnd);
                let cols = self.cols;
                for r in (row + 1)..self.rows {
                    self.screen_mut()[r].fill(Cell::default());
                    let _ = cols;
                }
            }
            EraseMode::ToStart => {
                let row = self.cursor_row;
                self.erase_line(EraseMode::ToStart);
                for r in 0..row {
                    self.screen_mut()[r].fill(Cell::default());
                }
            }
        }
    }

    // ── SGR state ────────────────────────────────────────────────────────

    pub fn reset_sgr(&mut self) {
        self.pending_fg = TermColor::Default;
        self.pending_bg = TermColor::Default;
        self.pending_attrs = Attrs::default();
    }

    pub fn set_fg(&mut self, color: TermColor) {
        self.pending_fg = color;
    }

    pub fn set_bg(&mut self, color: TermColor) {
        self.pending_bg = color;
    }

    pub fn set_attrs(&mut self, f: impl FnOnce(&mut Attrs)) {
        f(&mut self.pending_attrs);
    }

    // ── Screen mode ──────────────────────────────────────────────────────

    pub fn enter_alt_screen(&mut self) {
        if !self.on_alt_screen {
            self.on_alt_screen = true;
            let (rows, cols) = (self.rows, self.cols);
            self.alternate = vec![vec![Cell::default(); cols]; rows];
        }
    }

    pub fn leave_alt_screen(&mut self) {
        self.on_alt_screen = false;
    }
}
