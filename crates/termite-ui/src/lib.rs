// SPDX-License-Identifier: MIT
//! UI components and theme for Termite.
//!
//! Provides the colour palette, typography settings, and reusable Iced widgets
//! (TabBar, SidebarPanel, TerminalView, CommandPalette, HostCard, etc.).
//! Widgets are added incrementally as milestones are implemented.

pub mod prompt;
pub mod sidebar;
pub mod tabbar;
pub mod theme;

pub use prompt::{Prompt, PromptMessage};
pub use sidebar::{AuthKind, SidebarMessage, SidebarState};
pub use tabbar::{TabBarMessage, TabSummary};
pub use theme::TermiteTheme;
