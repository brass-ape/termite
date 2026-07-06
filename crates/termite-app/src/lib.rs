// SPDX-License-Identifier: MIT
//! Top-level application state and Iced wiring for Termite.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iced::keyboard::key::Named;
use iced::keyboard::{Key, Modifiers};
use iced::widget::text;
use iced::{Element, Font, Subscription, Task, Theme};

use termite_terminal::{GridHandler, Pty, TerminalGrid};

/// Default grid size until real window-size-driven resizing lands (M6).
const ROWS: usize = 30;
const COLS: usize = 100;

/// How often the output buffer is drained and fed to the VT parser.
const POLL_INTERVAL: Duration = Duration::from_millis(16);

// ── Application state ─────────────────────────────────────────────────────────

/// Root application state.
///
/// Extended in M1+ as sessions, host profiles, and UI state are added.
pub struct TermiteApp {
    grid: TerminalGrid,
    parser: vte::Parser,
    output: Arc<Mutex<Vec<u8>>>,
    writer: Box<dyn Write + Send>,
    /// Kept alive so the pty and child shell aren't torn down; unused
    /// otherwise until session lifecycle (M2+) needs it.
    _pty: Pty,
}

impl TermiteApp {
    fn new() -> Result<Self, termite_terminal::PtyError> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let pty = Pty::spawn(&shell, ROWS as u16, COLS as u16)?;

        let mut reader = pty.try_clone_reader()?;
        let writer = pty.take_writer()?;

        let output = Arc::new(Mutex::new(Vec::new()));
        let reader_output = Arc::clone(&output);
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Ok(mut output) = reader_output.lock() {
                            output.extend_from_slice(&buf[..n]);
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            grid: TerminalGrid::new(ROWS, COLS),
            parser: vte::Parser::new(),
            output,
            writer,
            _pty: pty,
        })
    }

    /// Feeds raw PTY output bytes through the VT parser into the grid.
    fn advance(&mut self, bytes: &[u8]) {
        let parser = &mut self.parser;
        let grid = &mut self.grid;
        let mut handler = GridHandler { grid };
        for &byte in bytes {
            parser.advance(&mut handler, byte);
        }
    }
}

// ── Messages ──────────────────────────────────────────────────────────────────

/// All messages that flow through the Iced update loop.
///
/// Extended in M1+ as features are introduced.
#[derive(Debug, Clone)]
pub enum Message {
    PollOutput,
    KeyPressed { key: Key, modifiers: Modifiers },
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Initialise logging and launch the Iced application.
pub fn run() -> iced::Result {
    init_tracing();

    iced::application("Termite", update, view)
        .theme(|_| Theme::Dark)
        .subscription(subscription)
        .run_with(initialize)
}

fn initialize() -> (TermiteApp, Task<Message>) {
    let app = TermiteApp::new().expect("failed to spawn local shell pty");
    (app, Task::none())
}

// ── Iced functions ────────────────────────────────────────────────────────────

fn update(app: &mut TermiteApp, message: Message) -> Task<Message> {
    match message {
        Message::PollOutput => {
            let bytes = {
                match app.output.lock() {
                    Ok(mut output) => std::mem::take(&mut *output),
                    Err(_) => Vec::new(),
                }
            };
            if !bytes.is_empty() {
                app.advance(&bytes);
            }
        }
        Message::KeyPressed { key, modifiers } => {
            if let Some(bytes) = key_to_bytes(&key, modifiers) {
                let _ = app.writer.write_all(&bytes);
            }
        }
    }
    Task::none()
}

fn view(app: &TermiteApp) -> Element<'_, Message> {
    let rows = app.grid.visible_rows().join("\n");
    text(rows).font(Font::MONOSPACE).size(14).into()
}

fn subscription(_app: &TermiteApp) -> Subscription<Message> {
    Subscription::batch([
        iced::time::every(POLL_INTERVAL).map(|_| Message::PollOutput),
        iced::keyboard::on_key_press(|key, modifiers| Some(Message::KeyPressed { key, modifiers })),
    ])
}

/// Converts a key press into the byte sequence to send to the shell.
fn key_to_bytes(key: &Key, modifiers: Modifiers) -> Option<Vec<u8>> {
    if modifiers.control() {
        if let Key::Character(c) = key {
            let c = c.chars().next()?;
            if c.is_ascii_alphabetic() {
                let byte = (c.to_ascii_lowercase() as u8) - b'a' + 1;
                return Some(vec![byte]);
            }
        }
    }

    match key {
        Key::Named(Named::Enter) => Some(vec![b'\r']),
        Key::Named(Named::Backspace) => Some(vec![0x7f]),
        Key::Named(Named::Tab) => Some(vec![b'\t']),
        Key::Named(Named::Escape) => Some(vec![0x1b]),
        Key::Named(Named::ArrowUp) => Some(b"\x1b[A".to_vec()),
        Key::Named(Named::ArrowDown) => Some(b"\x1b[B".to_vec()),
        Key::Named(Named::ArrowRight) => Some(b"\x1b[C".to_vec()),
        Key::Named(Named::ArrowLeft) => Some(b"\x1b[D".to_vec()),
        Key::Named(Named::Space) => Some(vec![b' ']),
        Key::Character(c) => Some(c.as_bytes().to_vec()),
        _ => None,
    }
}

// ── Logging setup ─────────────────────────────────────────────────────────────

fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("termite=info")),
        )
        .init();
}
