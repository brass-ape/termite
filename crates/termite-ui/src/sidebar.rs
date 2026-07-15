// SPDX-License-Identifier: MIT
//! The host list sidebar: shows saved [`HostProfile`]s and a small form for
//! adding/editing them. Pure presentation — the caller (`termite-app`) owns
//! the actual `HostStore`/`CredentialStore` and decides what each
//! `SidebarMessage` means.

use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::{Element, Length};

use termite_core::{HostId, HostProfile};

use crate::theme::colours;

/// Which authentication method the add/edit-host form currently has
/// selected. Kept separate from [`termite_core::AuthMethod`] because the
/// form needs a selectable "public key" state before a key path has been
/// typed in, which `AuthMethod::PublicKey`'s mandatory `PathBuf` can't
/// represent.
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
    TagsInputChanged(String),
    SearchInputChanged(String),
    /// The address field's Enter key was pressed — the caller should try to
    /// resolve it as a `~/.ssh/config` `Host` alias and, on a match, fill in
    /// whatever fields the user hasn't already typed over.
    ResolveAlias,
    /// Saves the form's contents: a new host when nothing is being edited,
    /// or an update to the profile named by the caller's own "editing"
    /// state when a host was opened via `EditHost`.
    SaveHost,
    /// Discards the form's contents and any in-progress edit.
    CancelEdit,
    /// Loads an existing host's fields into the form for editing.
    EditHost(HostId),
    DeleteHost(HostId),
    SelectHost(HostId),
    ToggleFavourite(HostId),
    /// Imports every literal (non-wildcard) `Host` alias from
    /// `~/.ssh/config` that isn't already a saved profile.
    ImportFromSshConfig,
    KeygenCommentChanged(String),
    KeygenPassphraseChanged(String),
    /// Generates a new ed25519 key pair and, on success, fills the key-path
    /// field with its location.
    GenerateKey,
}

/// The sidebar's own input state (the add/edit-host form and search box).
/// Persisted profiles themselves are owned by the caller, not this struct.
#[derive(Debug, Clone, Default)]
pub struct SidebarState {
    pub name_input: String,
    pub address_input: String,
    pub username_input: String,
    pub auth_kind: AuthKind,
    pub key_path_input: String,
    /// Comma-separated, exactly as typed — the caller splits/trims this
    /// into `HostProfile::tags` on save.
    pub tags_input: String,
    /// Filters the host list below by name/host/username/tag, case-
    /// insensitive substring match. Purely a display concern, so filtering
    /// itself happens in `view()` rather than needing caller involvement.
    pub search_input: String,
    /// `Some(id)` while the form is editing an existing host rather than
    /// creating a new one; set by the caller in response to `EditHost`.
    pub editing_id: Option<HostId>,
    /// Port resolved from `~/.ssh/config` for the current address, if
    /// `ResolveAlias` found one. There's no manual port field in this form
    /// (pre-existing gap, unrelated to alias resolution) — `None` means the
    /// caller should fall back to the default port 22.
    pub resolved_port: Option<u16>,
    /// Human-readable summary of the last successful alias resolution,
    /// shown under the address field. `None` when nothing has been
    /// resolved yet, or the address has changed since.
    pub resolved_hint: Option<String>,
    pub keygen_comment_input: String,
    pub keygen_passphrase_input: String,
}

/// Renders the sidebar: a search box, a scrollable host list, and a
/// compact add/edit-host form. `hosts` should already be in the caller's
/// desired display order — this only filters by `state.search_input`, it
/// does not reorder.
pub fn view<'a>(hosts: &'a [HostProfile], state: &'a SidebarState) -> Element<'a, SidebarMessage> {
    let query = state.search_input.to_ascii_lowercase();
    let visible = hosts.iter().filter(|host| {
        query.is_empty() || {
            let haystack = format!(
                "{} {} {} {}",
                host.name,
                host.host,
                host.username,
                host.tags.join(" ")
            )
            .to_ascii_lowercase();
            haystack.contains(&query)
        }
    });

    let list = visible.fold(column![].spacing(4), |col, host| col.push(host_row(host)));

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
        text_input(
            "host.example.com or a ~/.ssh/config alias",
            &state.address_input
        )
        .on_input(SidebarMessage::AddressInputChanged)
        .on_submit(SidebarMessage::ResolveAlias)
        .size(13),
        text_input("username", &state.username_input)
            .on_input(SidebarMessage::UsernameInputChanged)
            .size(13),
        text_input("tags (comma-separated)", &state.tags_input)
            .on_input(SidebarMessage::TagsInputChanged)
            .size(13),
        auth_picker,
    ]
    .spacing(6)
    .padding(8);

    if let Some(hint) = &state.resolved_hint {
        form = form.push(text(hint.clone()).size(11).color(colours::TEXT_MUTED));
    }

    if state.auth_kind == AuthKind::PublicKey {
        form = form.push(
            text_input("~/.ssh/id_ed25519", &state.key_path_input)
                .on_input(SidebarMessage::KeyPathInputChanged)
                .size(13),
        );
        form = form.push(
            column![
                text("or generate a new key:")
                    .size(11)
                    .color(colours::TEXT_MUTED),
                text_input("comment (optional)", &state.keygen_comment_input)
                    .on_input(SidebarMessage::KeygenCommentChanged)
                    .size(12),
                text_input("passphrase (optional)", &state.keygen_passphrase_input)
                    .on_input(SidebarMessage::KeygenPassphraseChanged)
                    .secure(true)
                    .size(12),
                button(text("Generate key").size(12)).on_press(SidebarMessage::GenerateKey),
            ]
            .spacing(4),
        );
    }

    let save_label = if state.editing_id.is_some() {
        "Save changes"
    } else {
        "Add host"
    };
    let mut buttons =
        row![button(text(save_label).size(13)).on_press(SidebarMessage::SaveHost)].spacing(6);
    if state.editing_id.is_some() {
        buttons = buttons.push(
            button(text("Cancel").size(13))
                .style(button::secondary)
                .on_press(SidebarMessage::CancelEdit),
        );
    }
    form = form.push(buttons);

    container(
        column![
            container(text("Hosts").size(14).color(colours::TEXT_MUTED)).padding(8),
            container(
                text_input("Search hosts...", &state.search_input)
                    .on_input(SidebarMessage::SearchInputChanged)
                    .size(12)
            )
            .padding([0, 8]),
            scrollable(list).height(Length::Fill),
            container(
                button(text("Import from ~/.ssh/config").size(12))
                    .style(button::secondary)
                    .on_press(SidebarMessage::ImportFromSshConfig)
            )
            .padding(8),
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

    // Plain ASCII rather than the ★/☆ glyphs: confirmed via a rendering
    // smoke test that iced's default UI font doesn't cover them, drawing a
    // missing-glyph box instead.
    let star = button(text(if host.favourite { "*" } else { "o" }).size(12).color(
        if host.favourite {
            colours::ACCENT
        } else {
            colours::TEXT_MUTED
        },
    ))
    .style(button::text)
    .on_press(SidebarMessage::ToggleFavourite(id));

    let mut info = column![
        text(host.name.clone()).size(13).color(colours::TEXT),
        text(format!("{}@{}", host.username, host.host))
            .size(11)
            .color(colours::TEXT_MUTED),
    ]
    .spacing(2);
    if !host.tags.is_empty() {
        info = info.push(
            text(host.tags.join(", "))
                .size(10)
                .color(colours::TEXT_MUTED),
        );
    }

    let label = button(info)
        .width(Length::Fill)
        .on_press(SidebarMessage::SelectHost(id));

    let edit = button(text("edit").size(11).color(colours::TEXT_MUTED))
        .style(button::text)
        .on_press(SidebarMessage::EditHost(id));

    let delete = button(text("x").size(12).color(colours::DESTRUCTIVE))
        .on_press(SidebarMessage::DeleteHost(id));

    container(row![star, label, edit, delete].spacing(4).padding([2, 8]))
        .width(Length::Fill)
        .into()
}
