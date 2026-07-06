// SPDX-License-Identifier: MIT
//! `vte::Perform` implementation that turns a VT byte stream into mutations
//! on a [`TerminalGrid`].

use vte::Params;

use crate::cell::TermColor;
use crate::grid::{EraseMode, TerminalGrid};

/// Bridges `vte`'s parser callbacks to [`TerminalGrid`] mutations.
///
/// Holds a mutable borrow of the grid rather than owning it, so the parser
/// and the grid can live as separate fields on the owning app state.
pub struct GridHandler<'a> {
    pub grid: &'a mut TerminalGrid,
}

/// Flattens a `Params` (semicolon-separated params, each possibly carrying
/// colon-separated subparams) into a single flat list of values.
fn flatten_params(params: &Params) -> Vec<u16> {
    params.iter().flatten().copied().collect()
}

/// Reads the value at `idx`, or `default` if absent.
fn param_at(values: &[u16], idx: usize, default: u16) -> u16 {
    values.get(idx).copied().unwrap_or(default)
}

/// Movement counts treat a `0` parameter the same as an absent one: both mean 1.
fn movement_count(values: &[u16], idx: usize) -> usize {
    match param_at(values, idx, 1) {
        0 => 1,
        n => n as usize,
    }
}

/// Parses the extended SGR colour forms `38/48;5;n` (indexed) and
/// `38/48;2;r;g;b` (true colour). `rest` is the params following the
/// `38`/`48` selector. Returns the parsed colour and how many further
/// elements (beyond the selector itself) it consumed.
fn parse_extended_color(rest: &[u16]) -> Option<(TermColor, usize)> {
    match rest.first().copied()? {
        5 => {
            let n = *rest.get(1)?;
            Some((TermColor::Indexed(n as u8), 2))
        }
        2 => {
            let r = *rest.get(1)?;
            let g = *rest.get(2)?;
            let b = *rest.get(3)?;
            Some((TermColor::Rgb(r as u8, g as u8, b as u8), 4))
        }
        _ => None,
    }
}

impl GridHandler<'_> {
    fn sgr(&mut self, params: &Params) {
        let values = flatten_params(params);
        if values.is_empty() {
            self.grid.reset_sgr();
            return;
        }

        let mut i = 0;
        while i < values.len() {
            match values[i] {
                0 => self.grid.reset_sgr(),
                1 => self.grid.set_attrs(|a| a.bold = true),
                2 => self.grid.set_attrs(|a| a.dim = true),
                3 => self.grid.set_attrs(|a| a.italic = true),
                4 => self.grid.set_attrs(|a| a.underline = true),
                7 => self.grid.set_attrs(|a| a.reverse = true),
                9 => self.grid.set_attrs(|a| a.strikethrough = true),
                22 => self.grid.set_attrs(|a| {
                    a.bold = false;
                    a.dim = false;
                }),
                23 => self.grid.set_attrs(|a| a.italic = false),
                24 => self.grid.set_attrs(|a| a.underline = false),
                27 => self.grid.set_attrs(|a| a.reverse = false),
                29 => self.grid.set_attrs(|a| a.strikethrough = false),
                n @ 30..=37 => self.grid.set_fg(TermColor::Indexed((n - 30) as u8)),
                38 => {
                    if let Some((color, consumed)) = parse_extended_color(&values[i + 1..]) {
                        self.grid.set_fg(color);
                        i += consumed;
                    }
                }
                39 => self.grid.set_fg(TermColor::Default),
                n @ 40..=47 => self.grid.set_bg(TermColor::Indexed((n - 40) as u8)),
                48 => {
                    if let Some((color, consumed)) = parse_extended_color(&values[i + 1..]) {
                        self.grid.set_bg(color);
                        i += consumed;
                    }
                }
                49 => self.grid.set_bg(TermColor::Default),
                n @ 90..=97 => self.grid.set_fg(TermColor::Indexed((n - 90 + 8) as u8)),
                n @ 100..=107 => self.grid.set_bg(TermColor::Indexed((n - 100 + 8) as u8)),
                _ => {}
            }
            i += 1;
        }
    }

    /// Handles CSI sequences with the `?` private-marker intermediate
    /// (DEC private modes). Only `?1049h`/`?1049l` (alternate screen) matter
    /// for M1.
    fn private_mode_dispatch(&mut self, params: &Params, action: char) {
        let values = flatten_params(params);
        if values.first() == Some(&1049) {
            match action {
                'h' => self.grid.enter_alt_screen(),
                'l' => self.grid.leave_alt_screen(),
                _ => {}
            }
        }
    }
}

impl vte::Perform for GridHandler<'_> {
    fn print(&mut self, c: char) {
        self.grid.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x0a => self.grid.linefeed(),
            0x0d => self.grid.carriage_return(),
            0x08 => self.grid.backspace(),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        if intermediates == b"?" {
            self.private_mode_dispatch(params, action);
            return;
        }

        let values = flatten_params(params);
        match action {
            'A' => self.grid.cursor_up(movement_count(&values, 0)),
            'B' => self.grid.cursor_down(movement_count(&values, 0)),
            'C' => self.grid.cursor_forward(movement_count(&values, 0)),
            'D' => self.grid.cursor_back(movement_count(&values, 0)),
            'H' | 'f' => {
                let row = param_at(&values, 0, 1).max(1) as usize;
                let col = param_at(&values, 1, 1).max(1) as usize;
                self.grid.set_cursor_position(row, col);
            }
            'J' => self
                .grid
                .erase_display(EraseMode::from_param(param_at(&values, 0, 0))),
            'K' => self
                .grid
                .erase_line(EraseMode::from_param(param_at(&values, 0, 0))),
            'm' => self.sgr(params),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        if intermediates.is_empty() {
            match byte {
                b'7' => self.grid.save_cursor(),
                b'8' => self.grid.restore_cursor(),
                _ => {}
            }
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if params.len() >= 2 && (params[0] == b"0" || params[0] == b"2") {
            if let Ok(title) = std::str::from_utf8(params[1]) {
                self.grid.set_title(title);
            }
        }
    }
}
