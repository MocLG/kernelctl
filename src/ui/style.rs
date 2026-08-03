/*
 * kernelctl — unified kernel and boot configuration management across Linux bootloaders.
 * Copyright (C) 2026 Luka Gejak
 *
 * This program is free software: you can redistribute it and/or modify it
 * under the terms of the GNU General Public License, version 3, as published
 * by the Free Software Foundation.
 *
 * This program is distributed in the hope that it will be useful, but WITHOUT
 * ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
 * FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for
 * more details.
 *
 * You should have received a copy of the GNU General Public License along
 * with this program. If not, see <https://www.gnu.org/licenses/>.
 *
 * Alternatively, this file is available under a commercial licence that lifts
 * the obligations of the GPL. Enquiries: lukagejak5@gmail.com
 */
//! Terminal styling for the non-interactive commands.
//!
//! Colour is emitted as raw ANSI rather than through a crate: the palette is
//! a dozen codes, and doing it here means colour decisions and the
//! when-to-colour policy live in one place.
//!
//! Output is only styled when it is going to a terminal that wants it.
//! Redirecting to a file or piping to another program yields clean text, so
//! `kernelctl list | grep` behaves the way a user expects.

use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether styling is enabled for this run. Set once at startup.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Decide whether to colour, honouring the usual overrides.
///
/// `force` reflects `--color=always`/`--no-color`; when it is `None` the
/// environment decides.
pub fn init(force: Option<bool>) {
    let enabled = match force {
        Some(v) => v,
        None => {
            // NO_COLOR is honoured whatever its value, per the convention;
            // CLICOLOR_FORCE overrides a non-tty destination.
            if std::env::var_os("NO_COLOR").is_some() {
                false
            } else if std::env::var_os("CLICOLOR_FORCE").is_some() {
                true
            } else if std::env::var("TERM").is_ok_and(|t| t == "dumb") {
                false
            } else {
                std::io::stdout().is_terminal()
            }
        }
    };
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// One SGR style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Bold,
    Dim,
    Red,
    Green,
    Yellow,
    Magenta,
    Cyan,
    BoldRed,
    BoldGreen,
    BoldYellow,
    BoldCyan,
    BoldWhite,
}

impl Style {
    fn codes(self) -> &'static str {
        match self {
            Style::Bold => "1",
            Style::Dim => "2",
            Style::Red => "31",
            Style::Green => "32",
            Style::Yellow => "33",
            Style::Magenta => "35",
            Style::Cyan => "36",
            Style::BoldRed => "1;31",
            Style::BoldGreen => "1;32",
            Style::BoldYellow => "1;33",
            Style::BoldCyan => "1;36",
            Style::BoldWhite => "1;37",
        }
    }
}

/// Wrap `text` in a style, or return it unchanged when colour is off.
pub fn paint(style: Style, text: &str) -> String {
    if enabled() {
        format!("\x1b[{}m{text}\x1b[0m", style.codes())
    } else {
        text.to_string()
    }
}

pub fn bold(text: &str) -> String {
    paint(Style::Bold, text)
}

pub fn dim(text: &str) -> String {
    paint(Style::Dim, text)
}

/// A state badge, coloured by what it means.
///
/// The colours carry meaning rather than decoration: green is the entry that
/// will boot, red is one that cannot.
pub fn badge(label: &str) -> String {
    let style = match label {
        "DEFAULT" => Style::BoldGreen,
        "ONESHOT" => Style::BoldYellow,
        "RUNNING" => Style::BoldCyan,
        "BROKEN" | "FOREIGN" => Style::BoldRed,
        "RECOVERY" => Style::Yellow,
        "EFI-STUB" | "UKI" => Style::Magenta,
        _ => Style::Dim,
    };
    paint(style, &format!("[{label}]"))
}

/// A section heading in `status` and `help`.
pub fn heading(text: &str) -> String {
    paint(Style::BoldWhite, text)
}

/// A `key:` label in a two-column listing.
pub fn label(text: &str) -> String {
    paint(Style::Cyan, text)
}

/// Prefix for a message that reports something went wrong.
pub fn error_prefix() -> String {
    paint(Style::BoldRed, "error:")
}

pub fn warn_prefix() -> String {
    paint(Style::BoldYellow, "warning:")
}

pub fn ok_prefix() -> String {
    paint(Style::BoldGreen, "ok:")
}

/// Strip ANSI escape sequences, so width can be measured on styled text.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        // Skip the sequence: ESC '[' ... final byte in @-~.
        if chars.next() != Some('[') {
            continue;
        }
        for c in chars.by_ref() {
            if ('@'..='~').contains(&c) {
                break;
            }
        }
    }
    out
}

/// Display width of a string, ignoring any styling it carries.
pub fn width(text: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    UnicodeWidthStr::width(strip_ansi(text).as_str())
}

/// Truncate to `max` display columns, adding an ellipsis when it had to cut.
///
/// Only applied to unstyled text: cutting inside an escape sequence would
/// leave the terminal in that colour for the rest of the line.
pub fn truncate(text: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if width(text) <= max || max == 0 {
        return text.to_string();
    }
    if max == 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > max - 1 {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// Truncate a cell that may carry styling.
///
/// Cutting inside an escape sequence would leave the terminal stuck in that
/// colour for the rest of the line, so styled text is returned unchanged.
/// That is safe in practice: the cells we style are badges and ids, which are
/// short and are never the column chosen to absorb a narrow terminal.
pub fn truncate_styled(text: &str, max: usize) -> String {
    if text.contains('\x1b') {
        text.to_string()
    } else {
        truncate(text, max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Styling is process-global, so tests that depend on it set it first.
    fn with_color<T>(on: bool, f: impl FnOnce() -> T) -> T {
        let previous = enabled();
        ENABLED.store(on, Ordering::Relaxed);
        let out = f();
        ENABLED.store(previous, Ordering::Relaxed);
        out
    }

    #[test]
    fn paints_only_when_enabled() {
        with_color(true, || {
            assert_eq!(paint(Style::Red, "x"), "\x1b[31mx\x1b[0m");
        });
        with_color(false, || {
            // Piped output must stay clean so grep and diff work on it.
            assert_eq!(paint(Style::Red, "x"), "x");
        });
    }

    #[test]
    fn strips_escape_sequences() {
        assert_eq!(strip_ansi("\x1b[1;32m[DEFAULT]\x1b[0m"), "[DEFAULT]");
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi(""), "");
    }

    #[test]
    fn measures_width_ignoring_styling() {
        with_color(true, || {
            let painted = badge("DEFAULT");
            assert!(painted.len() > 9, "should carry escape codes");
            assert_eq!(width(&painted), 9, "[DEFAULT] is nine columns");
        });
    }

    #[test]
    fn measures_wide_characters() {
        // CJK characters occupy two columns each.
        assert_eq!(width("日本語"), 6);
        assert_eq!(width("abc"), 3);
    }

    #[test]
    fn truncates_to_a_column_budget() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("truncate me please", 8), "truncat…");
        assert_eq!(width(&truncate("truncate me please", 8)), 8);
    }

    #[test]
    fn truncation_respects_wide_characters() {
        // Cutting mid-character would misalign every following column.
        let out = truncate("日本語テスト", 5);
        assert!(width(&out) <= 5);
    }

    #[test]
    fn badges_are_colour_coded_by_meaning() {
        with_color(true, || {
            // Green for what will boot, red for what cannot.
            assert!(badge("DEFAULT").contains("1;32"));
            assert!(badge("BROKEN").contains("1;31"));
        });
    }
}
