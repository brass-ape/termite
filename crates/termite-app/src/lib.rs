// SPDX-License-Identifier: MIT
//! Top-level application state and Iced wiring for Termite.

use iced::{Element, Task, Theme};

// ── Application state ─────────────────────────────────────────────────────────

/// Root application state.
///
/// Extended in M1+ as sessions, host profiles, and UI state are added.
#[derive(Debug, Default)]
pub struct TermiteApp;

// ── Messages ──────────────────────────────────────────────────────────────────

/// All messages that flow through the Iced update loop.
///
/// Extended in M1+ as features are introduced.
#[derive(Debug, Clone)]
pub enum Message {}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Initialise logging and launch the Iced application.
pub fn run() -> iced::Result {
    init_tracing();

    iced::application("Termite", update, view)
        .theme(|_| Theme::Dark)
        .run()
}

// ── Iced functions ────────────────────────────────────────────────────────────

fn update(_app: &mut TermiteApp, _message: Message) -> Task<Message> {
    Task::none()
}

fn view(_app: &TermiteApp) -> Element<'_, Message> {
    // M0 placeholder — real UI built in M1+
    iced::widget::text("Termite — M0 scaffold").size(24).into()
}

// ── Logging setup ─────────────────────────────────────────────────────────────

fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("termite=info")),
        )
        .init();
}
