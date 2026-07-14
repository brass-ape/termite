// SPDX-License-Identifier: MIT
//! UI components and theme for Termite.
//!
//! Provides the colour palette, typography settings, and reusable Iced widgets
//! (TabBar, SidebarPanel, TerminalView, CommandPalette, HostCard, etc.).
//! Widgets are added incrementally as milestones are implemented.

pub mod sidebar;
pub mod theme;

pub use sidebar::{SidebarMessage, SidebarState};
pub use theme::TermiteTheme;
