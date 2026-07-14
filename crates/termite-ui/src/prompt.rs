// SPDX-License-Identifier: MIT
//! A modal shown over the rest of the UI for the two session events that
//! need an explicit, unforgeable decision from the user: supplying a
//! password/passphrase, or approving a host key (`CLAUDE.md`'s no-silent-
//! accept invariant). Pure presentation — the caller (`termite-app`) decides
//! what a submitted credential or approval actually does; this module only
//! ever sees plain display strings, never SSH types or secret material.

use iced::widget::{button, column, container, row, text, text_input};
use iced::{Border, Color, Element, Length};

use crate::theme::colours;

/// What the modal is currently asking the user for.
#[derive(Debug, Clone)]
pub enum Prompt {
    /// A password or passphrase is needed. `label` names what it's for
    /// (e.g. "Password for alice@example.com" or a key fingerprint) and is
    /// the caller's responsibility to fill in — this module never sees the
    /// underlying `AuthChallenge`.
    Credential { label: String, input: String },
    /// The server's host key needs explicit approval. `warning` is set for
    /// a changed key (possible MITM) versus an unrecognised first-contact
    /// key, and drives the modal's colour/wording.
    HostKey {
        label: String,
        algorithm: String,
        fingerprint: String,
        warning: bool,
    },
}

/// Messages emitted by the modal. The parent decides how to act on them
/// (send an `AuthResponse`/`ApproveHostKey` command, clear the prompt).
#[derive(Debug, Clone)]
pub enum PromptMessage {
    InputChanged(String),
    Submit,
    Cancel,
    Approve,
    Reject,
}

/// Renders `prompt` as a centred card over a dimmed backdrop. Callers should
/// layer this on top of the rest of the view (e.g. with `iced::widget::Stack`)
/// only while a prompt is pending.
pub fn view(prompt: &Prompt) -> Element<'_, PromptMessage> {
    let card: Element<'_, PromptMessage> = match prompt {
        Prompt::Credential { label, input } => column![
            text(label.clone()).size(14).color(colours::TEXT),
            text_input("", input)
                .on_input(PromptMessage::InputChanged)
                .on_submit(PromptMessage::Submit)
                .secure(true)
                .size(13),
            row![
                button(text("Cancel").size(13)).on_press(PromptMessage::Cancel),
                button(text("Submit").size(13)).on_press(PromptMessage::Submit),
            ]
            .spacing(8),
        ]
        .spacing(10)
        .padding(16)
        .into(),
        Prompt::HostKey {
            label,
            algorithm,
            fingerprint,
            warning,
        } => {
            let title_colour = if *warning {
                colours::DESTRUCTIVE
            } else {
                colours::TEXT
            };
            column![
                text(label.clone()).size(14).color(title_colour),
                text(format!("{algorithm} {fingerprint}"))
                    .size(12)
                    .color(colours::TEXT_MUTED),
                row![
                    button(text("Reject").size(13)).on_press(PromptMessage::Reject),
                    button(text("Trust & continue").size(13)).on_press(PromptMessage::Approve),
                ]
                .spacing(8),
            ]
            .spacing(10)
            .padding(16)
            .into()
        }
    };

    let card = container(card)
        .width(Length::Fixed(360.0))
        .style(|_theme| container::Style {
            background: Some(colours::SURFACE_RAISED.into()),
            border: Border {
                color: colours::ACCENT,
                width: 1.0,
                radius: 6.0.into(),
            },
            ..container::Style::default()
        });

    container(card)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(|_theme| container::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.55).into()),
            ..container::Style::default()
        })
        .into()
}
