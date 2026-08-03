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
//! Limine.
//!
//! Limine has two config generations and both are still in the wild:
//!
//! - Modern `limine.conf`: entries open with `/Title`, nesting is expressed by
//!   repeating the slash (`//Child`), and settings are `key: value`.
//! - Legacy `limine.cfg`: entries open with `:Title` and settings are
//!   `KEY=VALUE`.
//!
//! Which one a file uses is detected from its content rather than its name,
//! since a `limine.conf` written before the syntax change still parses as the
//! legacy form.
//!
//! Paths carry a resource prefix naming the partition to read from -
//! `boot():/vmlinuz`, `guid(...)/vmlinuz`, `hdd(1:1):/vmlinuz` - which is
//! stripped to recover the filesystem path.

use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::model::{BootEntry, EntryFlags, LoaderKind};
use crate::sys::atomic::{self, WriteOutcome};

use super::{resolve_under, scan::BootRoots, Bootloader, Capabilities, Context, Timeout};

/// Which config generation a file is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    /// `/Entry` headers and `key: value` settings.
    Modern,
    /// `:Entry` headers and `KEY=VALUE` settings.
    Legacy,
}

impl Syntax {
    /// Guess the generation from the file's content.
    ///
    /// A `/`-prefixed entry header only exists in the modern syntax and a
    /// `:`-prefixed one only in the legacy syntax, so the first entry header
    /// in the file settles it.
    pub fn detect(text: &str) -> Syntax {
        for line in text.lines() {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            if t.starts_with('/') {
                return Syntax::Modern;
            }
            if t.starts_with(':') {
                return Syntax::Legacy;
            }
        }
        // No entries at all: assume modern, which is what a new install writes.
        Syntax::Modern
    }
}

/// One parsed Limine entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LimineEntry {
    pub title: String,
    /// Slash/colon depth; 1 is a top-level entry.
    pub depth: u8,
    pub protocol: Option<String>,
    pub kernel: Option<String>,
    pub cmdline: String,
    pub modules: Vec<String>,
    pub dtb: Option<String>,
    pub comment: Option<String>,
    /// Whether this is a directory that only contains other entries.
    pub is_directory: bool,
    /// Full tree path, e.g. `OSes/Arch Linux`, which is what `default_entry`
    /// may name.
    pub tree_path: String,
}

/// Strip Limine's `resource(args):` prefix from a path.
///
/// The prefix says *which partition* to read from, which is information we
/// cannot act on; the remainder is the path within it.
fn strip_resource(raw: &str) -> String {
    let raw = raw.trim();
    // Find the `):` that terminates the resource specifier. Searching for the
    // closing paren first avoids tripping over a colon inside the arguments,
    // as in `hdd(1:1):/path`.
    if let Some(close) = raw.find("):") {
        return raw[close + 2..].to_string();
    }
    raw.to_string()
}

/// Split `key: value` or `KEY=VALUE` depending on the syntax.
fn split_setting(line: &str, syntax: Syntax) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (k, v) = match syntax {
        Syntax::Modern => line.split_once(':')?,
        Syntax::Legacy => line.split_once('=')?,
    };
    Some((k.trim().to_ascii_lowercase(), v.trim().to_string()))
}

/// Parsed config: global settings plus entries.
#[derive(Debug, Clone, Default)]
pub struct LimineConfig {
    pub timeout: Option<String>,
    pub default_entry: Option<String>,
    pub entries: Vec<LimineEntry>,
}

/// Parse a Limine config of either generation.
pub fn parse(text: &str, syntax: Syntax) -> LimineConfig {
    let mut cfg = LimineConfig::default();
    // Titles of the currently open entry at each depth, for building the
    // slash-separated tree path a `default_entry` may refer to.
    let mut ancestry: Vec<String> = Vec::new();
    let marker = match syntax {
        Syntax::Modern => '/',
        Syntax::Legacy => ':',
    };

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if trimmed.starts_with(marker) {
            let depth = trimmed.chars().take_while(|c| *c == marker).count();
            let mut title = trimmed[depth..].trim().to_string();
            // A leading '+' marks a directory that starts expanded; it is
            // presentation, not part of the name.
            let expanded = title.starts_with('+');
            if expanded {
                title = title[1..].trim().to_string();
            }

            ancestry.truncate(depth.saturating_sub(1));
            ancestry.push(title.clone());

            cfg.entries.push(LimineEntry {
                title,
                depth: depth as u8,
                tree_path: ancestry.join("/"),
                ..Default::default()
            });
            continue;
        }

        let Some((key, value)) = split_setting(trimmed, syntax) else { continue };

        // Settings before the first entry header are global.
        let Some(entry) = cfg.entries.last_mut() else {
            match key.as_str() {
                "timeout" => cfg.timeout = Some(value),
                "default_entry" => cfg.default_entry = Some(value),
                _ => {}
            }
            continue;
        };

        match key.as_str() {
            "protocol" => entry.protocol = Some(value),
            // `path` is the modern spelling, `kernel_path` the legacy one.
            "path" | "kernel_path" => entry.kernel = Some(strip_resource(&value)),
            "cmdline" | "kernel_cmdline" => entry.cmdline = value,
            "module_path" => entry.modules.push(strip_resource(&value)),
            "dtb_path" => entry.dtb = Some(strip_resource(&value)),
            "comment" => entry.comment = Some(value),
            // Globals may legally appear after entries too.
            "timeout" => cfg.timeout = Some(value),
            "default_entry" => cfg.default_entry = Some(value),
            _ => {}
        }
    }

    // An entry with no boot protocol that has children is a directory.
    let depths: Vec<u8> = cfg.entries.iter().map(|e| e.depth).collect();
    for (i, entry) in cfg.entries.iter_mut().enumerate() {
        let has_child = depths.get(i + 1).is_some_and(|d| *d > entry.depth);
        entry.is_directory = has_child && entry.kernel.is_none();
    }

    cfg
}

pub struct Limine {
    config: PathBuf,
    boot_root: PathBuf,
    confidence: u8,
}

impl Limine {
    /// Config locations, in the order Limine itself searches them.
    const CANDIDATES: [&'static str; 4] =
        ["limine.conf", "limine/limine.conf", "limine.cfg", "limine/limine.cfg"];

    pub fn detect(roots: &BootRoots) -> Option<Limine> {
        for root in &roots.boot {
            for name in Self::CANDIDATES {
                let path = root.join(name);
                if !path.is_file() {
                    continue;
                }
                // The installed bootloader binary raises confidence above a
                // config file that might just be left over.
                let installed = ["limine-bios.sys", "EFI/BOOT/BOOTX64.EFI", "EFI/BOOT/BOOTAA64.EFI"]
                    .iter()
                    .any(|p| root.join(p).exists());
                return Some(Limine {
                    config: path,
                    boot_root: root.clone(),
                    confidence: if installed { 88 } else { 70 },
                });
            }
        }
        None
    }

    fn load(&self) -> Result<(LimineConfig, Syntax, String)> {
        let text = atomic::read_to_string(&self.config)?;
        let syntax = Syntax::detect(&text);
        Ok((parse(&text, syntax), syntax, text))
    }
}

/// Is this entry the one `default_entry` selects?
///
/// The value is either a 1-based index into the bootable entries or a
/// slash-separated tree path.
fn is_default(value: &str, index: usize, entry: &LimineEntry) -> bool {
    let value = value.trim();
    if let Ok(n) = value.parse::<usize>() {
        return n == index + 1;
    }
    entry.tree_path == value || entry.title == value
}

fn timeout_to_limine(t: Timeout) -> String {
    match t {
        Timeout::Immediate => "0".into(),
        Timeout::Seconds(n) => n.to_string(),
        // Limine spells "wait forever" as `no`.
        Timeout::Indefinite => "no".into(),
    }
}

fn timeout_from_limine(v: &str) -> Option<Timeout> {
    match v.trim().to_ascii_lowercase().as_str() {
        "no" => Some(Timeout::Indefinite),
        "0" => Some(Timeout::Immediate),
        other => other.parse::<u32>().ok().map(Timeout::Seconds),
    }
}

/// Set a global `key` in a Limine config, preserving entries and comments.
///
/// Globals must stay above the first entry header: a setting after one belongs
/// to that entry, so appending at the end would silently attach it to the last
/// entry instead of the file.
fn set_global(text: &str, syntax: Syntax, key: &str, value: &str) -> String {
    let sep = match syntax {
        Syntax::Modern => ": ",
        Syntax::Legacy => "=",
    };
    let marker = match syntax {
        Syntax::Modern => '/',
        Syntax::Legacy => ':',
    };
    let assignment = format!("{key}{sep}{value}");

    let mut out: Vec<String> = Vec::new();
    let mut replaced = false;
    let mut first_entry = None;

    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if first_entry.is_none() && trimmed.starts_with(marker) {
            first_entry = Some(i);
        }
        let is_key = first_entry.is_none()
            && split_setting(trimmed, syntax).is_some_and(|(k, _)| k == key);

        if is_key && !replaced {
            out.push(assignment.clone());
            replaced = true;
        } else if !is_key {
            out.push(line.to_string());
        }
    }

    if !replaced {
        // Insert just before the first entry header, or at the end if the file
        // has no entries yet.
        let at = first_entry.unwrap_or(out.len());
        out.insert(at, assignment);
    }

    let mut joined = out.join("\n");
    joined.push('\n');
    joined
}

impl Bootloader for Limine {
    fn kind(&self) -> LoaderKind {
        LoaderKind::Limine
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::SET_DEFAULT | Capabilities::TIMEOUT | Capabilities::EDIT_CMDLINE
    }

    fn confidence(&self) -> u8 {
        self.confidence
    }

    fn config_files(&self) -> Vec<PathBuf> {
        vec![self.config.clone()]
    }

    fn entries(&self, _ctx: &Context) -> Result<Vec<BootEntry>> {
        let (cfg, _, _) = self.load()?;
        let mut out = Vec::new();

        for (i, le) in cfg.entries.iter().enumerate() {
            let mut entry = BootEntry::new(
                LoaderKind::Limine,
                &self.config,
                &le.tree_path,
                &le.title,
            );
            entry.kernel = le.kernel.as_deref().map(|p| resolve_under(&self.boot_root, p));
            entry.initrds =
                le.modules.iter().map(|p| resolve_under(&self.boot_root, p)).collect();
            entry.devicetree = le.dtb.as_deref().map(|p| resolve_under(&self.boot_root, p));
            entry.cmdline = le.cmdline.clone();
            entry.depth = le.depth.saturating_sub(1);

            if le.is_directory {
                entry.flags.insert(EntryFlags::SUBMENU);
            }
            // A non-Linux protocol means Limine hands off to something else.
            if le.protocol.as_deref().is_some_and(|p| {
                let p = p.to_ascii_lowercase();
                p == "efi" || p == "bios" || p == "chainload"
            }) {
                entry.flags.insert(EntryFlags::CHAINLOAD);
            }
            if let Some(p) = &le.protocol {
                entry.extra.insert("protocol".into(), p.clone());
            }
            if let Some(c) = &le.comment {
                entry.extra.insert("comment".into(), c.clone());
            }
            if let Some(d) = &cfg.default_entry {
                if is_default(d, i, le) {
                    entry.flags.insert(EntryFlags::DEFAULT);
                }
            }
            out.push(entry);
        }

        Ok(out)
    }

    fn set_default(&self, ctx: &Context, entry: &BootEntry) -> Result<Vec<WriteOutcome>> {
        ctx.preflight_write("set-default", &self.config)?;
        if ctx.dry_run {
            return Ok(Vec::new());
        }
        let (_, syntax, text) = self.load()?;
        // Write the tree path rather than an index: an index silently selects
        // the wrong entry as soon as the config is reordered.
        let updated = set_global(&text, syntax, "default_entry", &entry.native_id);
        Ok(vec![atomic::write_atomic(&self.config, updated.as_bytes())?])
    }

    fn timeout(&self, _ctx: &Context) -> Result<Option<Timeout>> {
        let (cfg, _, _) = self.load()?;
        Ok(cfg.timeout.as_deref().and_then(timeout_from_limine))
    }

    fn set_timeout(&self, ctx: &Context, timeout: Timeout) -> Result<Vec<WriteOutcome>> {
        ctx.preflight_write("timeout", &self.config)?;
        if ctx.dry_run {
            return Ok(Vec::new());
        }
        let (_, syntax, text) = self.load()?;
        let updated = set_global(&text, syntax, "timeout", &timeout_to_limine(timeout));
        Ok(vec![atomic::write_atomic(&self.config, updated.as_bytes())?])
    }

    fn set_cmdline(&self, ctx: &Context, entry: &BootEntry, cmdline: &str) -> Result<Vec<WriteOutcome>> {
        ctx.preflight_write("cmdline set", &self.config)?;
        if ctx.dry_run {
            return Ok(Vec::new());
        }
        let (_, syntax, text) = self.load()?;
        let updated = set_entry_cmdline(&text, syntax, &entry.native_id, cmdline)?;
        Ok(vec![atomic::write_atomic(&self.config, updated.as_bytes())?])
    }
}

/// Replace the `cmdline` of one entry, identified by its tree path.
fn set_entry_cmdline(text: &str, syntax: Syntax, tree_path: &str, cmdline: &str) -> Result<String> {
    let (marker, sep, key) = match syntax {
        Syntax::Modern => ('/', ": ", "cmdline"),
        Syntax::Legacy => (':', "=", "KERNEL_CMDLINE"),
    };

    let mut ancestry: Vec<String> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    let mut in_target = false;
    let mut replaced = false;
    // Where to insert a cmdline if the target entry has none.
    let mut insert_at: Option<usize> = None;

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with(marker) {
            // Leaving the target entry without having found a cmdline line
            // means we must add one at the end of its block.
            if in_target && !replaced && insert_at.is_none() {
                insert_at = Some(out.len());
            }
            let depth = trimmed.chars().take_while(|c| *c == marker).count();
            let title = trimmed[depth..].trim().trim_start_matches('+').trim().to_string();
            ancestry.truncate(depth.saturating_sub(1));
            ancestry.push(title);
            in_target = ancestry.join("/") == tree_path;
            out.push(line.to_string());
            continue;
        }

        let is_cmdline = in_target
            && split_setting(trimmed, syntax)
                .is_some_and(|(k, _)| k == "cmdline" || k == "kernel_cmdline");

        if is_cmdline {
            if !replaced {
                let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
                out.push(format!("{indent}{key}{sep}{cmdline}"));
                replaced = true;
            }
        } else {
            out.push(line.to_string());
        }
    }

    if in_target && !replaced && insert_at.is_none() {
        insert_at = Some(out.len());
    }

    if !replaced {
        let at = insert_at.ok_or_else(|| {
            Error::validation(format!("entry '{tree_path}' is not in {}", "the Limine config"))
        })?;
        out.insert(at, format!("    {key}{sep}{cmdline}"));
    }

    let mut joined = out.join("\n");
    joined.push('\n');
    Ok(joined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loaders::testsupport::{fake_kernel, Fixture, TempTree};

    const MODERN: &str = "\
# Limine configuration
timeout: 5
default_entry: 2

/Arch Linux
    comment: Main system
    protocol: linux
    path: boot():/vmlinuz-linux
    cmdline: root=UUID=abc rw quiet
    module_path: boot():/initramfs-linux.img

/+Other systems
//Arch Linux (fallback)
    protocol: linux
    path: boot():/vmlinuz-linux
    cmdline: root=UUID=abc rw single
    module_path: boot():/initramfs-linux-fallback.img
";

    const LEGACY: &str = "\
TIMEOUT=3
DEFAULT_ENTRY=1

:Arch Linux
    PROTOCOL=linux
    KERNEL_PATH=boot():/vmlinuz-linux
    KERNEL_CMDLINE=root=UUID=abc rw
    MODULE_PATH=boot():/initramfs-linux.img
";

    #[test]
    fn detects_the_config_generation() {
        assert_eq!(Syntax::detect(MODERN), Syntax::Modern);
        assert_eq!(Syntax::detect(LEGACY), Syntax::Legacy);
        // Comments must not confuse the detection.
        assert_eq!(Syntax::detect("# just a comment\n"), Syntax::Modern);
    }

    #[test]
    fn strips_resource_prefixes_from_paths() {
        assert_eq!(strip_resource("boot():/vmlinuz"), "/vmlinuz");
        // A colon inside the arguments must not terminate the prefix early.
        assert_eq!(strip_resource("hdd(1:1):/boot/vmlinuz"), "/boot/vmlinuz");
        assert_eq!(strip_resource("guid(1234-5678):/Image"), "/Image");
        // A bare path is left alone.
        assert_eq!(strip_resource("/vmlinuz"), "/vmlinuz");
    }

    #[test]
    fn parses_modern_config() {
        let cfg = parse(MODERN, Syntax::Modern);
        assert_eq!(cfg.timeout.as_deref(), Some("5"));
        assert_eq!(cfg.default_entry.as_deref(), Some("2"));
        assert_eq!(cfg.entries.len(), 3);

        let arch = &cfg.entries[0];
        assert_eq!(arch.title, "Arch Linux");
        assert_eq!(arch.kernel.as_deref(), Some("/vmlinuz-linux"));
        assert_eq!(arch.cmdline, "root=UUID=abc rw quiet");
        assert_eq!(arch.modules, vec!["/initramfs-linux.img"]);
        assert_eq!(arch.depth, 1);
    }

    #[test]
    fn parses_nested_entries_and_directories() {
        let cfg = parse(MODERN, Syntax::Modern);
        let dir = &cfg.entries[1];
        // The '+' expansion marker is presentation, not part of the name.
        assert_eq!(dir.title, "Other systems");
        assert!(dir.is_directory);

        let child = &cfg.entries[2];
        assert_eq!(child.depth, 2);
        assert_eq!(child.tree_path, "Other systems/Arch Linux (fallback)");
        assert!(child.cmdline.contains("single"));
    }

    #[test]
    fn parses_legacy_config() {
        let cfg = parse(LEGACY, Syntax::Legacy);
        assert_eq!(cfg.timeout.as_deref(), Some("3"));
        let arch = &cfg.entries[0];
        assert_eq!(arch.title, "Arch Linux");
        assert_eq!(arch.kernel.as_deref(), Some("/vmlinuz-linux"));
        assert_eq!(arch.cmdline, "root=UUID=abc rw");
    }

    #[test]
    fn resolves_default_by_index_and_by_path() {
        let cfg = parse(MODERN, Syntax::Modern);
        // default_entry: 2 is 1-based, so it selects the second entry.
        assert!(is_default("2", 1, &cfg.entries[1]));
        assert!(!is_default("2", 0, &cfg.entries[0]));
        assert!(is_default("Other systems/Arch Linux (fallback)", 2, &cfg.entries[2]));
    }

    #[test]
    fn converts_timeout_values() {
        assert_eq!(timeout_from_limine("5"), Some(Timeout::Seconds(5)));
        assert_eq!(timeout_from_limine("no"), Some(Timeout::Indefinite));
        assert_eq!(timeout_from_limine("0"), Some(Timeout::Immediate));
        for t in [Timeout::Immediate, Timeout::Seconds(8), Timeout::Indefinite] {
            assert_eq!(timeout_from_limine(&timeout_to_limine(t)), Some(t));
        }
    }

    #[test]
    fn sets_a_global_above_the_first_entry() {
        let out = set_global(MODERN, Syntax::Modern, "timeout", "20");
        assert!(out.contains("timeout: 20"));
        assert!(!out.contains("timeout: 5"));
        // Entries and comments survive.
        assert!(out.contains("/Arch Linux"));
        assert!(out.contains("# Limine configuration"));

        let reparsed = parse(&out, Syntax::Modern);
        assert_eq!(reparsed.timeout.as_deref(), Some("20"));
        assert_eq!(reparsed.entries.len(), 3);
    }

    #[test]
    fn inserts_a_missing_global_before_the_entries() {
        let text = "/Arch\n    protocol: linux\n    path: boot():/vmlinuz\n";
        let out = set_global(text, Syntax::Modern, "timeout", "9");
        let reparsed = parse(&out, Syntax::Modern);
        // Placed after an entry header it would belong to that entry instead.
        assert_eq!(reparsed.timeout.as_deref(), Some("9"));
        assert_eq!(reparsed.entries.len(), 1);
    }

    #[test]
    fn edits_the_cmdline_of_one_entry_only() {
        let out = set_entry_cmdline(MODERN, Syntax::Modern, "Arch Linux", "root=UUID=abc rw debug")
            .unwrap();
        let cfg = parse(&out, Syntax::Modern);
        assert_eq!(cfg.entries[0].cmdline, "root=UUID=abc rw debug");
        // The nested entry keeps its own command line.
        assert_eq!(cfg.entries[2].cmdline, "root=UUID=abc rw single");
    }

    #[test]
    fn edits_a_nested_entry_by_tree_path() {
        let out = set_entry_cmdline(
            MODERN,
            Syntax::Modern,
            "Other systems/Arch Linux (fallback)",
            "root=UUID=abc rw emergency",
        )
        .unwrap();
        let cfg = parse(&out, Syntax::Modern);
        assert_eq!(cfg.entries[2].cmdline, "root=UUID=abc rw emergency");
        assert_eq!(cfg.entries[0].cmdline, "root=UUID=abc rw quiet");
    }

    #[test]
    fn adds_a_cmdline_to_an_entry_that_lacks_one() {
        let text = "/Arch\n    protocol: linux\n    path: boot():/vmlinuz\n";
        let out = set_entry_cmdline(text, Syntax::Modern, "Arch", "quiet").unwrap();
        assert_eq!(parse(&out, Syntax::Modern).entries[0].cmdline, "quiet");
    }

    // ---- fixture-backed -----------------------------------------------

    fn limine_tree(tag: &str) -> TempTree {
        let tree = TempTree::new(tag);
        tree.file("limine.conf", MODERN);
        fake_kernel(&tree, "vmlinuz-linux");
        fake_kernel(&tree, "initramfs-linux.img");
        fake_kernel(&tree, "initramfs-linux-fallback.img");
        tree
    }

    #[test]
    fn detects_limine_from_its_config() {
        let tree = limine_tree("limine-detect");
        let loader = Limine::detect(&tree.roots()).expect("limine detected");
        assert_eq!(loader.config, tree.path("limine.conf"));
    }

    #[test]
    fn scores_an_installed_loader_higher() {
        let tree = limine_tree("limine-installed");
        tree.file("limine-bios.sys", "binary");
        let loader = Limine::detect(&tree.roots()).unwrap();
        assert_eq!(loader.confidence, 88);
    }

    #[test]
    fn produces_normalized_entries() {
        let tree = limine_tree("limine-entries");
        let fx = Fixture::rooted(tree.roots());
        let loader = Limine::detect(&fx.roots).unwrap();

        let entries = loader.entries(&fx.context()).unwrap();
        assert_eq!(entries.len(), 3);

        let arch = &entries[0];
        assert_eq!(arch.title, "Arch Linux");
        assert_eq!(arch.kernel.as_ref().unwrap(), &tree.path("vmlinuz-linux"));
        assert_eq!(arch.initrds, vec![tree.path("initramfs-linux.img")]);
        assert_eq!(arch.extra.get("protocol").map(String::as_str), Some("linux"));

        // default_entry: 2 selects the second entry, which is the directory.
        assert!(entries[1].is_default());
        assert!(entries[1].flags.contains(EntryFlags::SUBMENU));
    }

    #[test]
    fn set_default_writes_a_tree_path_not_an_index() {
        let tree = limine_tree("limine-setdefault");
        let fx = Fixture::rooted(tree.roots());
        let loader = Limine::detect(&fx.roots).unwrap();
        let entries = loader.entries(&fx.context()).unwrap();
        let target = entries.iter().find(|e| e.title.contains("fallback")).unwrap();

        loader.set_default(&fx.context(), target).unwrap();

        let text = tree.read("limine.conf");
        // An index would select the wrong entry after any reordering.
        assert!(text.contains("default_entry: Other systems/Arch Linux (fallback)"));

        let reread = loader.entries(&fx.context()).unwrap();
        assert!(reread.iter().find(|e| e.title.contains("fallback")).unwrap().is_default());
    }

    #[test]
    fn round_trips_a_timeout_change() {
        let tree = limine_tree("limine-timeout");
        let fx = Fixture::rooted(tree.roots());
        let loader = Limine::detect(&fx.roots).unwrap();

        loader.set_timeout(&fx.context(), Timeout::Indefinite).unwrap();
        assert_eq!(loader.timeout(&fx.context()).unwrap(), Some(Timeout::Indefinite));
        assert!(tree.read("limine.conf").contains("timeout: no"));

        loader.set_timeout(&fx.context(), Timeout::Seconds(15)).unwrap();
        assert_eq!(loader.timeout(&fx.context()).unwrap(), Some(Timeout::Seconds(15)));
    }

    #[test]
    fn set_cmdline_survives_a_reparse() {
        let tree = limine_tree("limine-cmdline");
        let fx = Fixture::rooted(tree.roots());
        let loader = Limine::detect(&fx.roots).unwrap();
        let entries = loader.entries(&fx.context()).unwrap();
        let arch = entries.iter().find(|e| e.title == "Arch Linux").unwrap();

        loader.set_cmdline(&fx.context(), arch, "root=UUID=abc rw loglevel=7").unwrap();

        let reread = loader.entries(&fx.context()).unwrap();
        assert_eq!(reread[0].cmdline, "root=UUID=abc rw loglevel=7");
        assert_eq!(reread.len(), 3);
    }
}
