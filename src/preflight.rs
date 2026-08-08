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
//! What makes an entry unsafe to boot into.
//!
//! The rules live here rather than beside either front end, because the CLI
//! and the TUI had drifted: the CLI refused a disabled entry and the TUI did
//! not, so the same choice was rejected on one screen and accepted on the
//! other. Only the wording differs now - the CLI has a whole terminal to
//! explain itself in, the TUI has one footer line.

use crate::error::{Error, Result};
use crate::model::{BootEntry, EntryFlags};

/// Why an entry must not be made the default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// Files the entry names that are not on disk.
    Missing(Vec<String>),
    /// Marked disabled in the loader's own config, so it is never offered.
    Disabled,
    /// A container for other entries rather than something bootable.
    Submenu,
}

/// The first reason this entry cannot be booted, if any.
pub fn problem_with(entry: &BootEntry) -> Option<Problem> {
    // Only absolute paths are checked: a chainload entry pointing at another
    // disk has nothing for us to stat.
    let missing: Vec<String> = entry
        .referenced_files()
        .iter()
        .filter(|p| p.is_absolute() && !p.exists())
        .map(|p| p.display().to_string())
        .collect();

    if !missing.is_empty() {
        return Some(Problem::Missing(missing));
    }
    if entry.flags.contains(EntryFlags::DISABLED) {
        return Some(Problem::Disabled);
    }
    if entry.flags.contains(EntryFlags::SUBMENU) {
        return Some(Problem::Submenu);
    }
    None
}

/// Refuse the entry with the full explanation, for the command line.
pub fn check_detailed(entry: &BootEntry) -> Result<()> {
    match problem_with(entry) {
        None => Ok(()),
        Some(Problem::Missing(missing)) => Err(Error::validation(format!(
            "'{}' references {} that {} not exist:\n  {}\n\
             booting it would drop the machine to a firmware prompt",
            entry.title,
            if missing.len() == 1 { "a file" } else { "files" },
            if missing.len() == 1 { "does" } else { "do" },
            missing.join("\n  ")
        ))),
        Some(Problem::Disabled) => Err(Error::validation(format!(
            "'{}' is disabled in the bootloader config, so it will not be offered at boot; \
             remove its `disabled` line first",
            entry.title
        ))),
        Some(Problem::Submenu) => Err(Error::validation(format!(
            "'{}' is a submenu, not a bootable entry",
            entry.title
        ))),
    }
}

/// Refuse the entry in one line, for the footer of the interactive screen.
pub fn check_short(entry: &BootEntry) -> Result<()> {
    match problem_with(entry) {
        None => Ok(()),
        Some(Problem::Missing(missing)) => Err(Error::validation(format!(
            "{} is missing: booting this entry would fail",
            missing.join(", ")
        ))),
        Some(Problem::Disabled) => {
            Err(Error::validation("this entry is disabled and would not be offered at boot"))
        }
        Some(Problem::Submenu) => {
            Err(Error::validation("this is a submenu, not a boot entry"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LoaderKind;
    use std::path::{Path, PathBuf};

    fn entry(flags: EntryFlags, kernel: Option<PathBuf>) -> BootEntry {
        let mut e = BootEntry::new(LoaderKind::Grub2, Path::new("/etc/x"), "Test", "test");
        e.flags = flags;
        e.kernel = kernel;
        e
    }

    #[test]
    fn a_present_kernel_with_no_flags_is_bootable() {
        // /proc/self/exe always exists, so this needs no fixture on disk.
        let e = entry(EntryFlags::NONE, Some(PathBuf::from("/proc/self/exe")));
        assert_eq!(problem_with(&e), None);
        assert!(check_detailed(&e).is_ok());
        assert!(check_short(&e).is_ok());
    }

    #[test]
    fn a_missing_kernel_is_reported_before_anything_else() {
        let e = entry(EntryFlags::DISABLED, Some(PathBuf::from("/nonexistent/vmlinuz")));
        assert_eq!(
            problem_with(&e),
            Some(Problem::Missing(vec!["/nonexistent/vmlinuz".to_string()]))
        );
    }

    #[test]
    fn both_front_ends_refuse_exactly_the_same_entries() {
        // The TUI used to accept a disabled entry that the command line
        // refused; keeping one rule set is what stops that recurring.
        let cases = [
            entry(EntryFlags::DISABLED, Some(PathBuf::from("/proc/self/exe"))),
            entry(EntryFlags::SUBMENU, None),
            entry(EntryFlags::NONE, Some(PathBuf::from("/nonexistent/vmlinuz"))),
        ];
        for e in cases {
            assert!(problem_with(&e).is_some());
            assert!(check_detailed(&e).is_err());
            assert!(check_short(&e).is_err());
        }
    }
}
