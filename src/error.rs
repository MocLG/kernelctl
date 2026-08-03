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
//! Error types for kernelctl.
//!
//! The tool touches a lot of optional system state (missing bootloaders,
//! unreadable config files, absent EFI variables), so most operations
//! distinguish "this isn't present" from "this is broken". `Error` carries
//! enough context for the CLI to print an actionable message without a
//! backtrace.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    /// An I/O failure with the path that caused it attached.
    Io { path: Option<PathBuf>, source: io::Error },

    /// A config file was found but could not be understood.
    Parse { path: PathBuf, line: Option<usize>, message: String },

    /// No bootloader could be identified on this system.
    NoBootloader,

    /// The requested entry pattern matched nothing.
    EntryNotFound { pattern: String },

    /// The requested entry pattern matched more than one entry.
    AmbiguousEntry { pattern: String, matches: Vec<String> },

    /// The operation requires root and we are not root.
    NeedsRoot { action: String },

    /// The detected bootloader cannot do what was asked of it.
    Unsupported { loader: String, action: String },

    /// A pre-flight check rejected the operation before anything was written.
    Validation(String),

    /// An external helper binary (efibootmgr, grub-editenv, ...) is missing.
    MissingTool { tool: String, hint: String },

    /// An external helper binary ran but failed.
    ToolFailed { tool: String, status: String, stderr: String },

    /// Catch-all for conditions that are simply reported verbatim.
    Other(String),
}

impl Error {
    pub fn io(path: impl AsRef<Path>, source: io::Error) -> Self {
        Error::Io { path: Some(path.as_ref().to_path_buf()), source }
    }

    pub fn parse(path: impl AsRef<Path>, line: Option<usize>, message: impl Into<String>) -> Self {
        Error::Parse {
            path: path.as_ref().to_path_buf(),
            line,
            message: message.into(),
        }
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Error::Validation(message.into())
    }

    pub fn other(message: impl Into<String>) -> Self {
        Error::Other(message.into())
    }

    pub fn unsupported(loader: impl Into<String>, action: impl Into<String>) -> Self {
        Error::Unsupported { loader: loader.into(), action: action.into() }
    }

    pub fn needs_root(action: impl Into<String>) -> Self {
        Error::NeedsRoot { action: action.into() }
    }

    /// True when the error means "absent" rather than "broken". Discovery
    /// treats these as a miss and keeps probing other bootloaders instead of
    /// aborting the whole scan.
    pub fn is_not_found(&self) -> bool {
        match self {
            Error::Io { source, .. } => source.kind() == io::ErrorKind::NotFound,
            Error::NoBootloader | Error::EntryNotFound { .. } => true,
            Error::MissingTool { .. } => true,
            _ => false,
        }
    }

    /// Short remediation hint shown under the error message, when we have one.
    pub fn hint(&self) -> Option<String> {
        match self {
            Error::NeedsRoot { action } => {
                Some(format!("re-run as root, e.g. `sudo kernelctl {action}`"))
            }
            Error::NoBootloader => Some(
                "no known bootloader config was found; pass --boot-dir to point at a mounted ESP"
                    .into(),
            ),
            Error::AmbiguousEntry { matches, .. } => {
                Some(format!("matched {} entries; use a full id to disambiguate", matches.len()))
            }
            Error::MissingTool { hint, .. } => Some(hint.clone()),
            Error::Io { source, .. } if source.kind() == io::ErrorKind::PermissionDenied => {
                Some("permission denied; this path usually needs root".into())
            }
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { path: Some(p), source } => write!(f, "{}: {source}", p.display()),
            Error::Io { path: None, source } => write!(f, "{source}"),
            Error::Parse { path, line: Some(n), message } => {
                write!(f, "{}:{n}: {message}", path.display())
            }
            Error::Parse { path, line: None, message } => write!(f, "{}: {message}", path.display()),
            Error::NoBootloader => write!(f, "no supported bootloader detected on this system"),
            Error::EntryNotFound { pattern } => write!(f, "no boot entry matches '{pattern}'"),
            Error::AmbiguousEntry { pattern, matches } => {
                write!(f, "'{pattern}' is ambiguous between: {}", matches.join(", "))
            }
            Error::NeedsRoot { action } => {
                write!(f, "'{action}' modifies boot configuration and requires root")
            }
            Error::Unsupported { loader, action } => {
                write!(f, "{loader} does not support {action}")
            }
            Error::Validation(m) => write!(f, "{m}"),
            Error::MissingTool { tool, .. } => write!(f, "required helper '{tool}' not found in PATH"),
            Error::ToolFailed { tool, status, stderr } => {
                let detail = stderr.trim();
                if detail.is_empty() {
                    write!(f, "{tool} failed ({status})")
                } else {
                    write!(f, "{tool} failed ({status}): {detail}")
                }
            }
            Error::Other(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(source: io::Error) -> Self {
        Error::Io { path: None, source }
    }
}
