//! The Boot Loader Specification type-1 entry format.
//!
//! One file per entry under `<boot>/loader/entries/*.conf`, holding
//! whitespace-separated key/value pairs. systemd-boot is the best known
//! consumer, but Barebox and GRUB's blscfg module read the same files, so the
//! parser lives here and the adapters share it.
//!
//! Keys may repeat: `initrd` appears once per image (microcode first), and
//! `options` is concatenated across lines. Everything else takes the last
//! value seen.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::model::{Arch, BootEntry, EntryFlags, KernelVersion, LoaderKind};
use crate::sys::atomic;

use super::resolve_under;

/// A parsed type-1 entry file, still in its own vocabulary.
#[derive(Debug, Clone, Default)]
pub struct BlsEntry {
    pub title: Option<String>,
    pub version: Option<String>,
    pub machine_id: Option<String>,
    pub sort_key: Option<String>,
    pub linux: Option<String>,
    pub initrd: Vec<String>,
    pub options: Vec<String>,
    pub devicetree: Option<String>,
    pub devicetree_overlay: Vec<String>,
    pub architecture: Option<String>,
    /// `efi` names a bare EFI binary to chainload instead of a Linux kernel.
    pub efi: Option<String>,
    /// Keys we do not model, preserved for display.
    pub extra: BTreeMap<String, String>,
}

/// Split a BLS line into key and value.
///
/// The specification separates them by the first run of whitespace, and a
/// value may itself contain spaces (`options root=/dev/sda1 ro quiet`), so
/// this is deliberately not a general `key = value` parse.
fn split_line(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    match line.split_once(char::is_whitespace) {
        Some((k, v)) => Some((k, v.trim())),
        // A bare key with no value is legal and means "empty".
        None => Some((line, "")),
    }
}

/// Parse the contents of one entry file.
pub fn parse(text: &str) -> BlsEntry {
    let mut e = BlsEntry::default();
    for line in text.lines() {
        let Some((key, value)) = split_line(line) else { continue };
        match key.to_ascii_lowercase().as_str() {
            "title" => e.title = Some(value.to_string()),
            "version" => e.version = Some(value.to_string()),
            "machine-id" => e.machine_id = Some(value.to_string()),
            "sort-key" => e.sort_key = Some(value.to_string()),
            "linux" => e.linux = Some(value.to_string()),
            // Repeatable: each line adds another image, in load order.
            "initrd" => e.initrd.push(value.to_string()),
            // Repeatable: the spec says multiple options lines concatenate.
            "options" => e.options.push(value.to_string()),
            "devicetree" => e.devicetree = Some(value.to_string()),
            "devicetree-overlay" => {
                e.devicetree_overlay.extend(value.split_whitespace().map(str::to_string))
            }
            "architecture" => e.architecture = Some(value.to_string()),
            "efi" => e.efi = Some(value.to_string()),
            other => {
                e.extra.insert(other.to_string(), value.to_string());
            }
        }
    }
    e
}

/// Read and convert every `*.conf` under an entries directory.
///
/// `boot_root` is the partition the paths inside the entries are relative to -
/// for systemd-boot that is the ESP, which is usually the parent of the
/// `loader` directory but need not be.
pub fn load_dir(
    entries_dir: &Path,
    boot_root: &Path,
    loader: LoaderKind,
) -> Result<Vec<BootEntry>> {
    let mut out = Vec::new();

    let dir = match std::fs::read_dir(entries_dir) {
        Ok(d) => d,
        // An absent entries directory just means no type-1 entries.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(crate::error::Error::io(entries_dir, e)),
    };

    let mut paths: Vec<PathBuf> = dir
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("conf")))
        .collect();
    // The bootloader sorts entries itself, but a stable order here keeps our
    // output reproducible across runs.
    paths.sort();

    for path in paths {
        let text = atomic::read_to_string(&path)?;
        out.push(to_boot_entry(&parse(&text), &path, boot_root, loader));
    }
    Ok(out)
}

/// The entry's native identifier: its filename, which is what a
/// `default` pattern and the LoaderEntryOneShot variable both refer to.
pub fn native_id(path: &Path) -> String {
    path.file_name().unwrap_or_default().to_string_lossy().into_owned()
}

/// Map a parsed BLS entry onto the normalized model.
pub fn to_boot_entry(
    bls: &BlsEntry,
    path: &Path,
    boot_root: &Path,
    loader: LoaderKind,
) -> BootEntry {
    let id = native_id(path);

    // Fall back to the filename when an entry has no title, which is legal and
    // happens with hand-written entries.
    let title = bls.title.clone().unwrap_or_else(|| {
        path.file_stem().unwrap_or_default().to_string_lossy().into_owned()
    });

    let mut entry = BootEntry::new(loader, path, &id, title);

    entry.kernel = bls.linux.as_deref().map(|p| resolve_under(boot_root, p));
    entry.initrds = bls.initrd.iter().map(|p| resolve_under(boot_root, p)).collect();
    entry.devicetree = bls.devicetree.as_deref().map(|p| resolve_under(boot_root, p));
    // The spec says repeated options lines concatenate into one command line.
    entry.cmdline = bls.options.join(" ").trim().to_string();
    entry.version = bls.version.as_deref().and_then(KernelVersion::parse);

    if let Some(arch) = &bls.architecture {
        entry.arch = Arch::from_machine(arch);
    }

    // An `efi` key means the entry chainloads a binary rather than booting a
    // kernel the normal way. A UKI is exactly that, so tell them apart by the
    // conventional EFI/Linux location UKIs are installed into.
    if let Some(efi) = &bls.efi {
        let resolved = resolve_under(boot_root, efi);
        let looks_like_uki = efi.to_ascii_lowercase().contains("/linux/");
        entry.flags.insert(if looks_like_uki {
            EntryFlags::UNIFIED
        } else {
            EntryFlags::CHAINLOAD
        });
        entry.kernel = Some(resolved);
    }

    for (key, value) in [
        ("machine-id", &bls.machine_id),
        ("sort-key", &bls.sort_key),
    ] {
        if let Some(v) = value {
            entry.extra.insert(key.to_string(), v.clone());
        }
    }
    for (k, v) in &bls.extra {
        entry.extra.insert(k.clone(), v.clone());
    }

    entry
}

/// Rewrite the `options` line of an entry file, preserving everything else.
///
/// Editing in place rather than regenerating the file keeps comments, key
/// order and unknown keys intact - the file may well have been hand-written,
/// and rewriting it wholesale would quietly discard the parts we do not model.
pub fn rewrite_options(text: &str, new_options: &str) -> String {
    let mut out = Vec::new();
    let mut replaced = false;

    for line in text.lines() {
        let is_options =
            split_line(line).is_some_and(|(k, _)| k.eq_ignore_ascii_case("options"));
        if is_options {
            // The first options line becomes the new value; any further ones
            // are dropped, since they used to concatenate onto it.
            if !replaced {
                out.push(format!("options {new_options}"));
                replaced = true;
            }
        } else {
            out.push(line.to_string());
        }
    }

    // No options line to replace: append one.
    if !replaced {
        out.push(format!("options {new_options}"));
    }

    let mut joined = out.join("\n");
    joined.push('\n');
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARCH_ENTRY: &str = "\
# Created by: archinstall
title   Arch Linux
version 6.11.5-arch1-1
machine-id d3b07384d113edec49eaa6238ad5ff00
sort-key arch
linux   /vmlinuz-linux
initrd  /amd-ucode.img
initrd  /initramfs-linux.img
options root=UUID=1b0e4b1d-1 rw quiet
";

    #[test]
    fn parses_a_typical_entry() {
        let e = parse(ARCH_ENTRY);
        assert_eq!(e.title.as_deref(), Some("Arch Linux"));
        assert_eq!(e.version.as_deref(), Some("6.11.5-arch1-1"));
        assert_eq!(e.linux.as_deref(), Some("/vmlinuz-linux"));
        assert_eq!(e.initrd, vec!["/amd-ucode.img", "/initramfs-linux.img"]);
        assert_eq!(e.options, vec!["root=UUID=1b0e4b1d-1 rw quiet"]);
        assert_eq!(e.sort_key.as_deref(), Some("arch"));
    }

    #[test]
    fn keeps_initrd_order() {
        // Microcode must stay first; the kernel applies them in order.
        let e = parse("linux /vmlinuz\ninitrd /intel-ucode.img\ninitrd /initramfs.img\n");
        assert_eq!(e.initrd, vec!["/intel-ucode.img", "/initramfs.img"]);
    }

    #[test]
    fn concatenates_repeated_options_lines() {
        let e = parse("options root=/dev/sda1 ro\noptions quiet splash\n");
        assert_eq!(e.options.join(" "), "root=/dev/sda1 ro quiet splash");
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        let e = parse("# comment\n\n   \ntitle Test\n");
        assert_eq!(e.title.as_deref(), Some("Test"));
        assert!(e.extra.is_empty());
    }

    #[test]
    fn treats_keys_case_insensitively() {
        let e = parse("TITLE Upper\nLinux /vmlinuz\n");
        assert_eq!(e.title.as_deref(), Some("Upper"));
        assert_eq!(e.linux.as_deref(), Some("/vmlinuz"));
    }

    #[test]
    fn values_may_contain_spaces() {
        let e = parse("title Fedora Linux 40 (Workstation Edition)\n");
        assert_eq!(e.title.as_deref(), Some("Fedora Linux 40 (Workstation Edition)"));
    }

    #[test]
    fn preserves_unknown_keys() {
        let e = parse("title T\nunknown-key some value\n");
        assert_eq!(e.extra.get("unknown-key").map(String::as_str), Some("some value"));
    }

    #[test]
    fn converts_to_normalized_entry() {
        let bls = parse(ARCH_ENTRY);
        let entry = to_boot_entry(
            &bls,
            Path::new("/boot/loader/entries/arch.conf"),
            Path::new("/boot"),
            LoaderKind::SystemdBoot,
        );

        assert_eq!(entry.title, "Arch Linux");
        assert_eq!(entry.native_id, "arch.conf");
        assert_eq!(entry.cmdline, "root=UUID=1b0e4b1d-1 rw quiet");
        assert_eq!(entry.initrds.len(), 2);
        assert_eq!(entry.version.unwrap().raw, "6.11.5-arch1-1");
        assert_eq!(entry.extra.get("machine-id").unwrap(), "d3b07384d113edec49eaa6238ad5ff00");
    }

    #[test]
    fn falls_back_to_filename_when_untitled() {
        let entry = to_boot_entry(
            &parse("linux /vmlinuz\n"),
            Path::new("/boot/loader/entries/custom.conf"),
            Path::new("/boot"),
            LoaderKind::SystemdBoot,
        );
        assert_eq!(entry.title, "custom");
    }

    #[test]
    fn marks_efi_chainload_entries() {
        let entry = to_boot_entry(
            &parse("title Windows\nefi /EFI/Microsoft/Boot/bootmgfw.efi\n"),
            Path::new("/boot/loader/entries/windows.conf"),
            Path::new("/boot"),
            LoaderKind::SystemdBoot,
        );
        assert!(entry.flags.contains(EntryFlags::CHAINLOAD));
        assert!(!entry.flags.contains(EntryFlags::UNIFIED));
    }

    #[test]
    fn marks_uki_entries_as_unified() {
        let entry = to_boot_entry(
            &parse("title Arch UKI\nefi /EFI/Linux/arch-linux.efi\n"),
            Path::new("/boot/loader/entries/uki.conf"),
            Path::new("/boot"),
            LoaderKind::SystemdBoot,
        );
        assert!(entry.flags.contains(EntryFlags::UNIFIED));
    }

    #[test]
    fn rewrites_options_in_place() {
        let out = rewrite_options(ARCH_ENTRY, "root=UUID=1b0e4b1d-1 rw debug");
        assert!(out.contains("options root=UUID=1b0e4b1d-1 rw debug"));
        // Everything else must survive untouched.
        assert!(out.contains("# Created by: archinstall"));
        assert!(out.contains("title   Arch Linux"));
        assert!(out.contains("initrd  /amd-ucode.img"));
        assert!(!out.contains("rw quiet"));
    }

    #[test]
    fn collapses_multiple_options_lines_on_rewrite() {
        let out = rewrite_options("title T\noptions a b\noptions c d\n", "x y");
        assert_eq!(out.matches("options").count(), 1);
        assert!(out.contains("options x y"));
    }

    #[test]
    fn appends_options_when_absent() {
        let out = rewrite_options("title T\nlinux /vmlinuz\n", "quiet");
        assert!(out.contains("options quiet"));
        assert!(out.contains("linux /vmlinuz"));
    }

    #[test]
    fn rewrite_round_trips_through_the_parser() {
        let out = rewrite_options(ARCH_ENTRY, "root=/dev/sda2 ro");
        let reparsed = parse(&out);
        assert_eq!(reparsed.options, vec!["root=/dev/sda2 ro"]);
        assert_eq!(reparsed.title.as_deref(), Some("Arch Linux"));
        assert_eq!(reparsed.initrd.len(), 2);
    }
}
