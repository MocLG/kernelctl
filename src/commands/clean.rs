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
//! `kernelctl clean` - find and remove kernel files nothing boots.
//!
//! Boot partitions are small and a few stale kernels fill them, at which point
//! the *next* kernel upgrade fails. This finds the leftovers.
//!
//! Safety is the whole design here. Something is only a candidate for removal
//! when every one of these holds:
//!
//! - no boot entry from **any** detected loader references it,
//! - it is not the running kernel's version,
//! - it is not the newest installed version,
//! - its version could be determined at all - anything unparseable is left
//!   alone rather than guessed at.
//!
//! Getting this wrong deletes the kernel the machine boots from, so the checks
//! are deliberately conservative and the file list is always shown first.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, Result};
use crate::model::{BootEntry, KernelVersion};
use crate::ui::style;
use crate::util::time;

use super::{success, App};

/// One removable item, grouped by the kernel version it belongs to.
#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub version: String,
    pub paths: Vec<PathBuf>,
    pub size: u64,
}

/// Directories holding per-kernel files, and the module root.
const MODULES_DIR: &str = "/lib/modules";

/// Collect every version referenced by a boot entry.
fn referenced_versions(entries: &[BootEntry]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for entry in entries {
        if let Some(v) = &entry.version {
            out.insert(v.raw.clone());
        }
        // Also take the version from each referenced filename: an entry may
        // point at a kernel whose version field was never set.
        for path in entry.referenced_files() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if let Some(v) = KernelVersion::from_filename(name) {
                    out.insert(v.raw);
                }
            }
        }
    }
    out
}

/// Every file in the boot directories, grouped by the version in its name.
fn installed_by_version(app: &App) -> Vec<(KernelVersion, Vec<PathBuf>)> {
    let mut groups: Vec<(KernelVersion, Vec<PathBuf>)> = Vec::new();

    let add = |version: KernelVersion, path: PathBuf, groups: &mut Vec<(KernelVersion, Vec<PathBuf>)>| {
        match groups.iter_mut().find(|(v, _)| v.raw == version.raw) {
            Some((_, paths)) => {
                if !paths.contains(&path) {
                    paths.push(path);
                }
            }
            None => groups.push((version, vec![path])),
        }
    };

    // Kernel and initramfs images in the boot directories.
    for root in &app.roots.boot {
        let Ok(dir) = std::fs::read_dir(root) else { continue };
        for entry in dir.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
            // Only files whose names follow the per-kernel convention are
            // considered; anything else is not ours to reason about.
            if !is_versioned_boot_file(name) {
                continue;
            }
            if let Some(v) = KernelVersion::from_filename(name) {
                add(v, path, &mut groups);
            }
        }
    }

    // Module directories.
    if let Ok(dir) = std::fs::read_dir(MODULES_DIR) {
        for entry in dir.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
            if let Some(v) = KernelVersion::parse(name) {
                add(v, path, &mut groups);
            }
        }
    }

    // Newest first, so `--keep N` keeps the newest N.
    groups.sort_by(|a, b| b.0.cmp(&a.0));
    groups
}

/// Does this filename follow the per-kernel naming convention?
fn is_versioned_boot_file(name: &str) -> bool {
    const PREFIXES: [&str; 9] = [
        "vmlinuz-",
        "vmlinux-",
        "bzImage-",
        "zImage-",
        "Image-",
        "initramfs-",
        "initrd.img-",
        "initrd-",
        "System.map-",
    ];
    PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Total size of a file or directory tree.
fn size_of(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return 0 };
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    std::fs::read_dir(path)
        .map(|dir| dir.flatten().map(|e| size_of(&e.path())).sum())
        .unwrap_or(0)
}

/// Work out what is safe to remove.
pub fn find_candidates(app: &App, entries: &[BootEntry], keep: usize) -> Vec<Candidate> {
    let referenced = referenced_versions(entries);
    let installed = installed_by_version(app);

    // The newest installed version is never a candidate even if no entry
    // references it yet - a kernel installed but not yet added to the boot
    // menu is the normal state midway through an upgrade.
    let newest = installed.first().map(|(v, _)| v.raw.clone());

    installed
        .iter()
        .skip(keep)
        .filter(|(version, _)| {
            if referenced.contains(&version.raw) {
                return false;
            }
            if app.host.is_running_release(&version.raw) {
                return false;
            }
            if newest.as_deref() == Some(version.raw.as_str()) {
                return false;
            }
            true
        })
        .map(|(version, paths)| Candidate {
            version: version.raw.clone(),
            paths: paths.clone(),
            size: paths.iter().map(|p| size_of(p)).sum(),
        })
        .collect()
}

pub fn run(app: &App, keep: usize, list_only: bool) -> Result<()> {
    // Every loader's entries count as references, not just the primary one:
    // a kernel still listed in a leftover GRUB config is one the user can
    // still choose at boot, so it is not garbage.
    let ctx = app.context();
    let (entries, errors) = app.discovery.all_entries(&ctx);

    // A config we could not read might reference anything, so removing files
    // on the strength of an incomplete picture is not safe.
    if !errors.is_empty() {
        for (kind, err) in &errors {
            super::warn(&format!("{kind}: {err}"));
        }
        return Err(Error::validation(
            "some bootloader configuration could not be read, so it is not possible to \
             tell which kernels are still in use; fix or remove it before cleaning",
        ));
    }

    let candidates = find_candidates(app, &entries, keep);

    if app.args.json {
        return super::print_json(&candidates);
    }

    if candidates.is_empty() {
        println!("nothing to clean: every installed kernel is referenced, running, or newest");
        return Ok(());
    }

    let total: u64 = candidates.iter().map(|c| c.size).sum();
    let file_count: usize = candidates.iter().map(|c| c.paths.len()).sum();

    println!("{}", style::heading("Unreferenced kernel files"));
    for candidate in &candidates {
        println!(
            "\n  {} {}",
            style::bold(&candidate.version),
            style::dim(&format!("({})", time::format_bytes(candidate.size)))
        );
        for path in &candidate.paths {
            println!("    {}", path.display());
        }
    }

    println!();
    println!(
        "{} file{} across {} kernel version{}, {} total",
        file_count,
        if file_count == 1 { "" } else { "s" },
        candidates.len(),
        if candidates.len() == 1 { "" } else { "s" },
        style::bold(&time::format_bytes(total))
    );

    if list_only {
        return Ok(());
    }

    println!();
    println!(
        "{}",
        style::dim(&format!(
            "the running kernel ({}), the newest installed kernel, and anything a boot \
             entry references are excluded",
            app.host.kernel_release
        ))
    );

    app.privileges.require("clean")?;

    if app.args.dry_run {
        super::dry_run_notice(&format!("remove {file_count} files, freeing {}", time::format_bytes(total)));
        return Ok(());
    }

    if !app.confirm(&format!("Remove these {file_count} files?"))? {
        println!("cancelled");
        return Ok(());
    }

    let mut removed = 0usize;
    let mut freed = 0u64;

    for candidate in &candidates {
        for path in &candidate.paths {
            let size = size_of(path);
            let result = if path.is_dir() {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            };
            match result {
                Ok(()) => {
                    removed += 1;
                    freed += size;
                    if app.args.verbose {
                        println!("  {} {}", style::dim("removed"), path.display());
                    }
                }
                Err(e) => super::warn(&format!("could not remove {}: {e}", path.display())),
            }
        }
    }

    success(&format!(
        "removed {removed} file{}, freeing {}",
        if removed == 1 { "" } else { "s" },
        time::format_bytes(freed)
    ));

    if app.discovery.kinds().contains(&crate::model::LoaderKind::Grub2) {
        super::note_line("run `grub-mkconfig -o /boot/grub/grub.cfg` to drop the removed kernels from the menu");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LoaderKind;

    #[test]
    fn recognises_per_kernel_filenames() {
        assert!(is_versioned_boot_file("vmlinuz-6.11.0-9-generic"));
        assert!(is_versioned_boot_file("initrd.img-6.11.0-9-generic"));
        assert!(is_versioned_boot_file("initramfs-6.11.0.img"));
        assert!(is_versioned_boot_file("System.map-6.11.0"));

        // Unversioned names are not ours to reason about and must be left be.
        assert!(!is_versioned_boot_file("vmlinuz"));
        assert!(!is_versioned_boot_file("grub.cfg"));
        assert!(!is_versioned_boot_file("memtest86+.bin"));
        assert!(!is_versioned_boot_file("efi"));
    }

    #[test]
    fn collects_versions_from_entries_and_their_files() {
        let mut a = BootEntry::new(LoaderKind::Grub2, "/boot/grub/grub.cfg", "a", "Linux");
        a.version = KernelVersion::parse("6.11.0-9-generic");

        // This entry has no version field, so it must be recovered from the
        // kernel filename instead.
        let mut b = BootEntry::new(LoaderKind::Grub2, "/boot/grub/grub.cfg", "b", "Older");
        b.kernel = Some(PathBuf::from("/boot/vmlinuz-6.10.0-5-generic"));

        let refs = referenced_versions(&[a, b]);
        assert!(refs.contains("6.11.0-9-generic"));
        assert!(refs.contains("6.10.0-5-generic"));
    }

    #[test]
    fn an_entry_with_neither_version_nor_files_contributes_nothing() {
        let e = BootEntry::new(LoaderKind::Lilo, "/etc/lilo.conf", "win", "Windows");
        assert!(referenced_versions(&[e]).is_empty());
    }

    #[test]
    fn size_of_sums_a_directory_tree() {
        let tree = crate::loaders::testsupport::TempTree::new("clean-size");
        tree.file("mods/a", "12345");
        tree.file("mods/sub/b", "678");
        assert_eq!(size_of(&tree.path("mods")), 8);
        assert_eq!(size_of(&tree.path("mods/a")), 5);
    }

    #[test]
    fn size_of_a_missing_path_is_zero() {
        assert_eq!(size_of(Path::new("/nonexistent/kernelctl/path")), 0);
    }
}
