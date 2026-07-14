// SPDX-License-Identifier: MIT
//! The host list sidebar: shows saved [`HostProfile`]s and a small form for
//! adding new ones. Pure presentation — the caller (`termite-app`) owns the
//! actual `HostStore` and decides what `SidebarMessage` means.

use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::{Element, Length};

use termite_core::{HostId, HostProfile};

use crate::theme::colours;

/// Which authentication method the add-host form currently has selected.
/// Kept separate from [`termite_core::AuthMethod`] because the form needs a
/// selectable "public key" state before a key path has been typed in, which
/// `AuthMethod::PublicKey`'s mandatory `PathBuf` can't represent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthKind {
    #[default]
    Agent,
    Password,
    PublicKey,
}

impl AuthKind {
    const ALL: [AuthKind; 3] = [AuthKind::Agent, AuthKind::Password, AuthKind::PublicKey];

    fn label(self) -> &'static str {
        match self {
            AuthKind::Agent => "Agent",
            AuthKind::Password => "Password",
            AuthKind::PublicKey => "Public key",
        }
    }
}

/// Messages emitted by the sidebar. The parent maps these into its own
/// top-level message type and decides how to act on them (e.g. persisting
/// via a `HostStore`).
#[derive(Debug, Clone)]
pub enum SidebarMessage {
    NameInputChanged(String),
    AddressInputChanged(String),
    UsernameInputChanged(String),
    AuthKindSelected(AuthKind),
    KeyPathInputChanged(String),
    AddHost,
    DeleteHost(HostId),
    SelectHost(HostId),
}

/// The sidebar's own input state (the new-host form). Persisted profiles
/// themselves are owned by the caller, not this struct.
#[derive(Debug, Clone, Default)]
pub struct SidebarState {
    pub name_input: String,
    pub address_input: String,
    pub username_input: String,
    pub auth_kind: AuthKind,
    pub key_path_input: String,
}

/// Renders the sidebar: a scrollable host list above a compact "add host"
/// form. `hosts` should already be in the caller's desired display order.
pub fn view<'a>(hosts: &'a [HostProfile], state: &'a SidebarState) -> Element<'a, SidebarMessage> {
    let list = hosts
        .iter()
        .fold(column![].spacing(4), |col, host| col.push(host_row(host)));

    let auth_picker = AuthKind::ALL.iter().fold(row![].spacing(4), |row, &kind| {
        let label = text(kind.label()).size(12);
        let picker = if kind == state.auth_kind {
            button(label.color(colours::TEXT)).style(button::primary)
        } else {
            button(label.color(colours::TEXT_MUTED)).style(button::secondary)
        };
        row.push(picker.on_press(SidebarMessage::AuthKindSelected(kind)))
    });

    let mut form = column![
        text_input("Name", &state.name_input)
            .on_input(SidebarMessage::NameInputChanged)
            .size(13),
        text_input("host.example.com", &state.address_input)
            .on_input(SidebarMessage::AddressInputChanged)
            .size(13),
        text_input("username", &state.username_input)
            .on_input(SidebarMessage::UsernameInputChanged)
            .size(13),
        auth_picker,
    ]
    .spacing(6)
    .padding(8);

    if state.auth_kind == AuthKind::PublicKey {
        form = form.push(
            text_input("~/.ssh/id_ed25519", &state.key_path_input)
                .on_input(SidebarMessage::KeyPathInputChanged)
                .size(13),
        );
    }

    form = form.push(button(text("Add host").size(13)).on_press(SidebarMessage::AddHost));

    container(
        column![
            container(text("Hosts").size(14).color(colours::TEXT_MUTED)).padding(8),
            scrollable(list).height(Length::Fill),
            form,
        ]
        .width(Length::Fixed(220.0)),
    )
    .style(|_theme| container::Style {
        background: Some(colours::SURFACE.into()),
        ..container::Style::default()
    })
    .height(Length::Fill)
    .into()
}

fn host_row(host: &HostProfile) -> Element<'_, SidebarMessage> {
    let id = host.id;
    let label = button(
        column![
            text(host.name.clone()).size(13).color(colours::TEXT),
            text(format!("{}@{}", host.username, host.host))
                .size(11)
                .color(colours::TEXT_MUTED),
        ]
        .spacing(2),
    )
    .width(Length::Fill)
    .on_press(SidebarMessage::SelectHost(id));

    let delete = button(text("x").size(12).color(colours::DESTRUCTIVE))
        .on_press(SidebarMessage::DeleteHost(id));

    container(row![label, delete].spacing(4).padding([2, 8]))
        .width(Length::Fill)
        .into()
}
