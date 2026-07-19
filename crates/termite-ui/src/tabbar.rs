// SPDX-License-Identifier: MIT
//! The tab bar: shows every open tab (local shells and SSH sessions) with a
//! status indicator, and lets the caller switch, close, retry, or open new
//! ones. Pure presentation, mirroring `sidebar.rs`'s contract — the caller
//! owns tab/session state and decides what each `TabBarMessage` means.

use iced::widget::{button, row, text};
use iced::{Color, Element};

use termite_core::{ConnectionStatus, TabId};

use crate::theme::colours;

/// Messages emitted by the tab bar. The parent maps these into its own
/// top-level message type.
#[derive(Debug, Clone)]
pub enum TabBarMessage {
    Select(TabId),
    Close(TabId),
    /// Manually retry a tab whose automatic reconnect attempts have been
    /// exhausted, or that's `Disconnected`/`Failed` for any other reason.
    Retry(TabId),
    /// Opens a new local shell tab.
    NewLocal,
}

/// The caller-owned display data for one open tab. `termite-ui` has no
/// knowledge of SSH sessions or the local PTY — just enough to render a row.
#[derive(Debug, Clone)]
pub struct TabSummary {
    pub id: TabId,
    pub title: String,
    pub status: ConnectionStatus,
}

/// Renders the tab bar: one row per open tab plus a trailing "+" button for
/// a new local shell tab. `tabs` should already be in the caller's desired
/// display order — this only renders, it doesn't reorder.
///
/// Returns a `'static` `Element`: unlike `sidebar::view` (which borrows
/// `&str` fields directly into `text_input` widgets), everything here is
/// cloned into owned data or `Copy`, so the result doesn't need to borrow
/// `tabs` — letting the caller build `tabs` as a fresh, short-lived `Vec`
/// each frame instead of needing it to live as long as its own state.
pub fn view(tabs: &[TabSummary], active: Option<TabId>) -> Element<'static, TabBarMessage> {
    let mut bar = row![].spacing(2).padding(4);
    for tab in tabs {
        bar = bar.push(tab_row(tab, Some(tab.id) == active));
    }
    bar = bar.push(
        button(text("+").size(14))
            .style(button::text)
            .on_press(TabBarMessage::NewLocal),
    );
    bar.into()
}

fn tab_row(tab: &TabSummary, is_active: bool) -> Element<'static, TabBarMessage> {
    let (glyph, glyph_color) = status_indicator(&tab.status);
    let title_color = if is_active {
        colours::TEXT
    } else {
        colours::TEXT_MUTED
    };

    let label = button(
        row![
            text(glyph).size(11).color(glyph_color),
            text(tab.title.clone()).size(12).color(title_color),
        ]
        .spacing(6),
    )
    .style(if is_active {
        button::primary
    } else {
        button::secondary
    })
    .on_press(TabBarMessage::Select(tab.id));

    let mut controls = row![label].spacing(2);

    if matches!(
        tab.status,
        ConnectionStatus::Disconnected | ConnectionStatus::Failed { .. }
    ) {
        controls = controls.push(
            button(text("retry").size(10).color(colours::ACCENT))
                .style(button::text)
                .on_press(TabBarMessage::Retry(tab.id)),
        );
    }

    controls = controls.push(
        button(text("x").size(11).color(colours::TEXT_MUTED))
            .style(button::text)
            .on_press(TabBarMessage::Close(tab.id)),
    );

    controls.into()
}

/// A short ASCII status glyph and its colour — plain tokens rather than
/// unicode symbols, since iced's default UI font is missing glyphs like the
/// `★`/`☆` the M4 favourite-star toggle hit (see `HANDOFF.md`).
fn status_indicator(status: &ConnectionStatus) -> (&'static str, Color) {
    match status {
        ConnectionStatus::Connecting => ("...", colours::TEXT_MUTED),
        ConnectionStatus::Connected => ("*", colours::SUCCESS),
        ConnectionStatus::Reconnecting { .. } => ("~", colours::ACCENT),
        ConnectionStatus::Disconnected => ("o", colours::TEXT_MUTED),
        ConnectionStatus::Failed { .. } => ("!", colours::DESTRUCTIVE),
    }
}
