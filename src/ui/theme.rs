#![allow(dead_code)] // palette is consumed incrementally across milestones
//! Gruvbox dark palette. Every color in the UI must come from here.
//! Reference: https://github.com/morhetz/gruvbox (dark, medium contrast).

use ratatui::style::{Color, Modifier, Style};

pub const BG0_H: Color = Color::Rgb(0x1d, 0x20, 0x21);
pub const BG0: Color = Color::Rgb(0x28, 0x28, 0x28);
pub const BG1: Color = Color::Rgb(0x3c, 0x38, 0x36);
pub const BG2: Color = Color::Rgb(0x50, 0x49, 0x45);
pub const BG3: Color = Color::Rgb(0x66, 0x5c, 0x54);
pub const FG0: Color = Color::Rgb(0xfb, 0xf1, 0xc7);
pub const FG: Color = Color::Rgb(0xeb, 0xdb, 0xb2);
pub const FG4: Color = Color::Rgb(0xa8, 0x99, 0x84);
pub const GRAY: Color = Color::Rgb(0x92, 0x83, 0x74);

pub const RED: Color = Color::Rgb(0xfb, 0x49, 0x34);
pub const GREEN: Color = Color::Rgb(0xb8, 0xbb, 0x26);
pub const YELLOW: Color = Color::Rgb(0xfa, 0xbd, 0x2f);
pub const BLUE: Color = Color::Rgb(0x83, 0xa5, 0x98);
pub const PURPLE: Color = Color::Rgb(0xd3, 0x86, 0x9b);
pub const AQUA: Color = Color::Rgb(0x8e, 0xc0, 0x7c);
pub const ORANGE: Color = Color::Rgb(0xfe, 0x80, 0x19);
/// Gruvbox-toned dark backgrounds for changed lines.
pub const ADDED_BG: Color = Color::Rgb(0x32, 0x36, 0x1a);
pub const REMOVED_BG: Color = Color::Rgb(0x3c, 0x1f, 0x1e);

/// Semantic roles. Widgets use these, not raw colors.
pub struct Theme;

impl Theme {
    pub fn base() -> Style {
        Style::new().fg(FG).bg(BG0)
    }
    pub fn border() -> Style {
        Style::new().fg(BG3)
    }
    pub fn border_focused() -> Style {
        Style::new().fg(YELLOW)
    }
    pub fn title() -> Style {
        Style::new().fg(YELLOW).add_modifier(Modifier::BOLD)
    }
    pub fn selected() -> Style {
        Style::new().fg(FG0).bg(BG2).add_modifier(Modifier::BOLD)
    }
    pub fn muted() -> Style {
        Style::new().fg(GRAY)
    }
    pub fn status_bar() -> Style {
        Style::new().fg(FG4).bg(BG1)
    }
    pub fn key_hint() -> Style {
        Style::new().fg(ORANGE).add_modifier(Modifier::BOLD)
    }
    /// Script can run on this kernel.
    pub fn ok() -> Style {
        Style::new().fg(GREEN)
    }
    /// Some probes missing / needs --unsafe / unknown.
    pub fn warn() -> Style {
        Style::new().fg(YELLOW)
    }
    /// Cannot run here.
    pub fn error() -> Style {
        Style::new().fg(RED)
    }
    pub fn running() -> Style {
        Style::new().fg(AQUA).add_modifier(Modifier::BOLD)
    }
    pub fn hist_bar() -> Style {
        Style::new().fg(BLUE)
    }
    pub fn accent() -> Style {
        Style::new().fg(PURPLE)
    }
    /// A line the user added in an inline edit (background only, keeps highlighting).
    pub fn diff_added_line() -> Style {
        Style::new().bg(ADDED_BG)
    }
    pub fn diff_added_marker() -> Style {
        Style::new().fg(GREEN).bg(ADDED_BG).add_modifier(Modifier::BOLD)
    }
    /// A line of the original that the edit removed.
    pub fn diff_removed_line() -> Style {
        Style::new()
            .fg(FG4)
            .bg(REMOVED_BG)
            .add_modifier(Modifier::CROSSED_OUT)
    }
    pub fn diff_removed_marker() -> Style {
        Style::new().fg(RED).bg(REMOVED_BG).add_modifier(Modifier::BOLD)
    }
    pub fn popup_bg() -> Style {
        Style::new().fg(FG).bg(BG0_H)
    }
    /// Field names in the detail pane ("Probes", "File", …).
    pub fn label() -> Style {
        Style::new().fg(FG4).add_modifier(Modifier::BOLD)
    }
    pub fn tab_active() -> Style {
        Style::new().fg(YELLOW).add_modifier(Modifier::BOLD)
    }
    pub fn tab_inactive() -> Style {
        Style::new().fg(GRAY)
    }
    /// A target that cannot run anything (no bpftrace, connection lost): stands out.
    pub fn tab_broken() -> Style {
        Style::new().fg(RED).add_modifier(Modifier::BOLD)
    }
    /// Persistent full-width warning (kernel lockdown).
    pub fn banner_error() -> Style {
        Style::new().fg(BG0_H).bg(RED).add_modifier(Modifier::BOLD)
    }
    /// Privilege badge in the status bar.
    pub fn badge_ok() -> Style {
        Style::new().fg(GREEN).bg(BG1).add_modifier(Modifier::BOLD)
    }
    pub fn badge_warn() -> Style {
        Style::new().fg(YELLOW).bg(BG1).add_modifier(Modifier::BOLD)
    }
    pub fn badge_error() -> Style {
        Style::new().fg(RED).bg(BG1).add_modifier(Modifier::BOLD)
    }
    pub fn notice_info() -> Style {
        Style::new().fg(AQUA).bg(BG1)
    }
    pub fn notice_warn() -> Style {
        Style::new().fg(YELLOW).bg(BG1)
    }
    pub fn notice_error() -> Style {
        Style::new().fg(RED).bg(BG1).add_modifier(Modifier::BOLD)
    }

    // Top table deltas vs the previous snapshot. Neutral hues: up/down is not good/bad.
    pub fn delta_up() -> Style {
        Style::new().fg(YELLOW)
    }
    pub fn delta_down() -> Style {
        Style::new().fg(AQUA)
    }
    pub fn delta_new() -> Style {
        Style::new().fg(PURPLE).add_modifier(Modifier::BOLD)
    }

    // Source view (light syntax highlighting).
    pub fn line_number() -> Style {
        Style::new().fg(BG3)
    }
    pub fn code_comment() -> Style {
        Style::new().fg(GRAY).add_modifier(Modifier::ITALIC)
    }
    pub fn code_string() -> Style {
        Style::new().fg(GREEN)
    }
    pub fn code_probe() -> Style {
        Style::new().fg(AQUA).add_modifier(Modifier::BOLD)
    }
    pub fn code_builtin() -> Style {
        Style::new().fg(ORANGE)
    }
    pub fn code_map() -> Style {
        Style::new().fg(PURPLE)
    }
    pub fn code_var() -> Style {
        Style::new().fg(BLUE)
    }
}
