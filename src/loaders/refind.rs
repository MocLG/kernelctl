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
//! rEFInd.
//!
//! `refind.conf` mixes global directives with brace-delimited `menuentry`
//! blocks. Values are optionally quoted, and `options` almost always is,
//! because a kernel command line contains spaces.
//!
//! rEFInd also auto-scans for bootable images, so the config usually
//! describes only the entries a user pinned by hand. That is a genuine limit:
//! entries rEFInd discovers at boot time do not exist anywhere on disk for us
//! to read, so only the manual ones are listed.

use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::model::{BootEntry, EntryFlags, LoaderKind};
use crate::sys::atomic::{self, WriteOutcome};

use super::{resolve_under, scan::BootRoots, Bootloader, Capabilities, Context, Timeout};

/// One `menuentry` block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefindEntry {
    pub title: String,
    pub loader: Option<String>,
    pub initrds: Vec<String>,
    pub options: String,
    pub volume: Option<String>,
    pub icon: Option<String>,
    pub disabled: bool,
    /// Line index of the `options` directive, for in-place edits.
    pub options_line: Option<usize>,
    /// Line index of the `menuentry` line, so options can be inserted.
    pub header_line: usize,
}

/// Strip one layer of matching quotes.
fn unquote(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2 {
        let b = v.as_bytes();
        if (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
            return v[1..v.len() - 1].to_string();
        }
    }
    v.to_string()
}

/// Split a rEFInd directive into keyword and value.
fn split_directive(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    // A lone closing brace is structure, not a directive.
    if trimmed == "}" {
        return None;
    }
    let without_brace = trimmed.trim_end_matches('{').trim();
    match without_brace.split_once(char::is_whitespace) {
        Some((k, v)) => Some((k.to_ascii_lowercase(), v.trim().to_string())),
        None => Some((without_brace.to_ascii_lowercase(), String::new())),
    }
}

#[derive(Debug, Clone, Default)]
pub struct RefindConfig {
    pub timeout: Option<i64>,
    pub default_selection: Option<String>,
    pub entries: Vec<RefindEntry>,
}

/// Parse a refind.conf.
pub fn parse(text: &str) -> RefindConfig {
    let mut cfg = RefindConfig::default();
    // Depth 0 is the global section; 1 is inside a menuentry; 2+ is inside a
    // submenuentry, whose directives we deliberately do not merge upward.
    let mut depth = 0usize;

    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim();

        if trimmed == "}" {
            depth = depth.saturating_sub(1);
            continue;
        }

        let Some((keyword, value)) = split_directive(line) else { continue };
        let opens = trimmed.ends_with('{');

        if keyword == "menuentry" && depth == 0 {
            cfg.entries.push(RefindEntry {
                title: unquote(&value),
                header_line: i,
                ..Default::default()
            });
            depth += 1;
            continue;
        }
        if opens {
            // submenuentry, or any other nested block.
            depth += 1;
            continue;
        }

        if depth == 0 {
            match keyword.as_str() {
                "timeout" => cfg.timeout = value.trim().parse().ok(),
                "default_selection" => cfg.default_selection = Some(unquote(&value)),
                _ => {}
            }
            continue;
        }

        // Only directives directly inside a menuentry describe that entry;
        // deeper ones belong to a submenu variant.
        if depth != 1 {
            continue;
        }
        let Some(entry) = cfg.entries.last_mut() else { continue };
        match keyword.as_str() {
            "loader" => entry.loader = Some(unquote(&value)),
            "initrd" => entry.initrds.push(unquote(&value)),
            "options" => {
                entry.options = unquote(&value);
                entry.options_line = Some(i);
            }
            "volume" => entry.volume = Some(unquote(&value)),
            "icon" => entry.icon = Some(unquote(&value)),
            "disabled" => entry.disabled = true,
            _ => {}
        }
    }

    cfg
}

/// Does `default_selection` name this entry?
///
/// rEFInd matches a substring of the title, and also accepts a 1-based index.
fn is_default(selection: &str, index: usize, entry: &RefindEntry) -> bool {
    let selection = selection.trim();
    if selection.is_empty() || selection == "+" {
        // `+` means "whatever booted last", which is runtime state we cannot
        // read from the config.
        return false;
    }
    if let Ok(n) = selection.parse::<usize>() {
        return n == index + 1;
    }
    entry.title.contains(selection)
}

fn timeout_to_refind(t: Timeout) -> String {
    match t {
        Timeout::Immediate => "0".into(),
        Timeout::Seconds(n) => n.to_string(),
        // rEFInd uses -1 for "show the menu until a key is pressed".
        Timeout::Indefinite => "-1".into(),
    }
}

fn timeout_from_refind(v: i64) -> Timeout {
    match v {
        0 => Timeout::Immediate,
        n if n < 0 => Timeout::Indefinite,
        n => Timeout::Seconds(n as u32),
    }
}

pub struct Refind {
    config: PathBuf,
    boot_root: PathBuf,
}

impl Refind {
    const CANDIDATES: [&'static str; 3] =
        ["EFI/refind/refind.conf", "EFI/BOOT/refind.conf", "refind.conf"];

    pub fn detect(roots: &BootRoots) -> Option<Refind> {
        for root in &roots.boot {
            for name in Self::CANDIDATES {
                let path = root.join(name);
                if path.is_file() {
                    return Some(Refind { config: path, boot_root: root.clone() });
                }
            }
        }
        None
    }

    fn load(&self) -> Result<(RefindConfig, String)> {
        let text = atomic::read_to_string(&self.config)?;
        Ok((parse(&text), text))
    }
}

/// Set a global directive, keeping it above the first menuentry.
fn set_global(text: &str, keyword: &str, value: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut replaced = false;
    let mut first_entry: Option<usize> = None;

    for line in text.lines() {
        let trimmed = line.trim();
        if first_entry.is_none() && trimmed.starts_with("menuentry") {
            first_entry = Some(out.len());
        }
        let is_target = first_entry.is_none()
            && split_directive(line).is_some_and(|(k, _)| k == keyword);

        if is_target && !replaced {
            out.push(format!("{keyword} {value}"));
            replaced = true;
        } else if !is_target {
            out.push(line.to_string());
        }
    }

    if !replaced {
        out.insert(first_entry.unwrap_or(out.len()), format!("{keyword} {value}"));
    }

    let mut joined = out.join("\n");
    joined.push('\n');
    joined
}

impl Bootloader for Refind {
    fn kind(&self) -> LoaderKind {
        LoaderKind::Refind
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::SET_DEFAULT | Capabilities::TIMEOUT | Capabilities::EDIT_CMDLINE
    }

    fn confidence(&self) -> u8 {
        85
    }

    fn config_files(&self) -> Vec<PathBuf> {
        vec![self.config.clone()]
    }

    fn post_write_note(&self) -> Option<String> {
        Some(
            "rEFInd also auto-scans for bootable images at boot; entries it finds that way \
             are not in refind.conf and cannot be listed or changed here"
                .to_string(),
        )
    }

    fn entries(&self, _ctx: &Context) -> Result<Vec<BootEntry>> {
        let (cfg, _) = self.load()?;
        let selection = cfg.default_selection.clone().unwrap_or_default();

        Ok(cfg
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.disabled)
            .map(|(i, re)| {
                let mut entry =
                    BootEntry::new(LoaderKind::Refind, &self.config, &re.title, &re.title);

                entry.kernel = re.loader.as_deref().map(|p| resolve_under(&self.boot_root, p));
                entry.initrds =
                    re.initrds.iter().map(|p| resolve_under(&self.boot_root, p)).collect();
                entry.cmdline = re.options.clone();

                if let Some(v) = &re.volume {
                    entry.extra.insert("volume".into(), v.clone());
                }
                if let Some(icon) = &re.icon {
                    entry.extra.insert("icon".into(), icon.clone());
                }
                // A loader that is not a Linux kernel is a chainload.
                if re.loader.as_deref().is_some_and(|l| l.to_ascii_lowercase().ends_with(".efi")) {
                    entry.flags.insert(EntryFlags::EFI_STUB);
                }
                if is_default(&selection, i, re) {
                    entry.flags.insert(EntryFlags::DEFAULT);
                }
                entry
            })
            .collect())
    }

    fn set_default(&self, ctx: &Context, entry: &BootEntry) -> Result<Vec<WriteOutcome>> {
        ctx.preflight_write("set-default", &self.config)?;
        if ctx.dry_run {
            return Ok(Vec::new());
        }
        let (_, text) = self.load()?;
        let updated = set_global(&text, "default_selection", &format!("\"{}\"", entry.native_id));
        Ok(vec![atomic::write_atomic(&self.config, updated.as_bytes())?])
    }

    fn timeout(&self, _ctx: &Context) -> Result<Option<Timeout>> {
        let (cfg, _) = self.load()?;
        Ok(cfg.timeout.map(timeout_from_refind))
    }

    fn set_timeout(&self, ctx: &Context, timeout: Timeout) -> Result<Vec<WriteOutcome>> {
        ctx.preflight_write("timeout", &self.config)?;
        if ctx.dry_run {
            return Ok(Vec::new());
        }
        let (_, text) = self.load()?;
        let updated = set_global(&text, "timeout", &timeout_to_refind(timeout));
        Ok(vec![atomic::write_atomic(&self.config, updated.as_bytes())?])
    }

    fn set_cmdline(&self, ctx: &Context, entry: &BootEntry, cmdline: &str) -> Result<Vec<WriteOutcome>> {
        ctx.preflight_write("cmdline set", &self.config)?;
        let (cfg, text) = self.load()?;
        let target = cfg
            .entries
            .iter()
            .find(|e| e.title == entry.native_id)
            .ok_or_else(|| Error::EntryNotFound { pattern: entry.native_id.clone() })?;

        if ctx.dry_run {
            return Ok(Vec::new());
        }

        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        // Always re-quote: a rEFInd command line contains spaces and would
        // otherwise be read as several directives.
        let quoted = format!("\"{}\"", cmdline.replace('"', "\\\""));

        match target.options_line {
            Some(n) => {
                let indent: String =
                    lines[n].chars().take_while(|c| c.is_whitespace()).collect();
                lines[n] = format!("{indent}options {quoted}");
            }
            None => lines.insert((target.header_line + 1).min(lines.len()), format!("    options {quoted}")),
        }

        let mut joined = lines.join("\n");
        joined.push('\n');
        Ok(vec![atomic::write_atomic(&self.config, joined.as_bytes())?])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loaders::testsupport::{fake_kernel, Fixture, TempTree};

    const CONF: &str = r#"
# rEFInd configuration
timeout 20
default_selection "Arch Linux"
scanfor manual,external

menuentry "Arch Linux" {
    icon /EFI/refind/icons/os_arch.png
    volume "ESP"
    loader /vmlinuz-linux
    initrd /initramfs-linux.img
    options "root=UUID=abc rw quiet"
    submenuentry "Boot with fallback initramfs" {
        initrd /initramfs-linux-fallback.img
    }
}

menuentry "Windows" {
    loader /EFI/Microsoft/Boot/bootmgfw.efi
}

menuentry "Retired" {
    loader /vmlinuz-old
    disabled
}
"#;

    #[test]
    fn parses_entries_and_globals() {
        let cfg = parse(CONF);
        assert_eq!(cfg.timeout, Some(20));
        assert_eq!(cfg.default_selection.as_deref(), Some("Arch Linux"));
        assert_eq!(cfg.entries.len(), 3);

        let arch = &cfg.entries[0];
        assert_eq!(arch.title, "Arch Linux");
        assert_eq!(arch.loader.as_deref(), Some("/vmlinuz-linux"));
        assert_eq!(arch.options, "root=UUID=abc rw quiet");
        assert_eq!(arch.volume.as_deref(), Some("ESP"));
    }

    #[test]
    fn submenu_directives_do_not_leak_into_the_parent() {
        let cfg = parse(CONF);
        // The fallback initrd belongs to the submenuentry, not to the entry.
        assert_eq!(cfg.entries[0].initrds, vec!["/initramfs-linux.img"]);
    }

    #[test]
    fn recognises_disabled_entries() {
        let cfg = parse(CONF);
        assert!(cfg.entries[2].disabled);
        assert!(!cfg.entries[0].disabled);
    }

    #[test]
    fn matches_default_by_substring_and_index() {
        let cfg = parse(CONF);
        assert!(is_default("Arch", 0, &cfg.entries[0]));
        assert!(is_default("2", 1, &cfg.entries[1]));
        // `+` means "last booted", which is runtime state, not config.
        assert!(!is_default("+", 0, &cfg.entries[0]));
    }

    #[test]
    fn converts_timeout_values() {
        assert_eq!(timeout_from_refind(20), Timeout::Seconds(20));
        assert_eq!(timeout_from_refind(0), Timeout::Immediate);
        assert_eq!(timeout_from_refind(-1), Timeout::Indefinite);
        assert_eq!(timeout_to_refind(Timeout::Indefinite), "-1");
    }

    #[test]
    fn sets_a_global_above_the_entries() {
        let out = set_global(CONF, "timeout", "5");
        let cfg = parse(&out);
        assert_eq!(cfg.timeout, Some(5));
        assert_eq!(cfg.entries.len(), 3);
        assert!(out.contains("# rEFInd configuration"));
    }

    // ---- fixture-backed ---------------------------------------------

    fn refind_tree(tag: &str) -> TempTree {
        let tree = TempTree::new(tag);
        tree.file("EFI/refind/refind.conf", CONF);
        fake_kernel(&tree, "vmlinuz-linux");
        fake_kernel(&tree, "initramfs-linux.img");
        tree
    }

    #[test]
    fn detects_refind_and_skips_disabled_entries() {
        let tree = refind_tree("refind-detect");
        let fx = Fixture::rooted(tree.roots());
        let loader = Refind::detect(&fx.roots).expect("refind detected");

        let entries = loader.entries(&fx.context()).unwrap();
        // The disabled entry is not offered at boot, so it is not listed.
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.title != "Retired"));
        assert!(entries[0].is_default());
        assert_eq!(entries[0].kernel.as_ref().unwrap(), &tree.path("vmlinuz-linux"));
    }

    #[test]
    fn set_default_and_timeout_round_trip() {
        let tree = refind_tree("refind-write");
        let fx = Fixture::rooted(tree.roots());
        let loader = Refind::detect(&fx.roots).unwrap();
        let entries = loader.entries(&fx.context()).unwrap();
        let windows = entries.iter().find(|e| e.title == "Windows").unwrap();

        loader.set_default(&fx.context(), windows).unwrap();
        loader.set_timeout(&fx.context(), Timeout::Indefinite).unwrap();

        let reread = loader.entries(&fx.context()).unwrap();
        assert!(reread.iter().find(|e| e.title == "Windows").unwrap().is_default());
        assert!(!reread.iter().find(|e| e.title == "Arch Linux").unwrap().is_default());
        assert_eq!(loader.timeout(&fx.context()).unwrap(), Some(Timeout::Indefinite));
    }

    #[test]
    fn set_cmdline_requotes_and_survives_a_reparse() {
        let tree = refind_tree("refind-cmdline");
        let fx = Fixture::rooted(tree.roots());
        let loader = Refind::detect(&fx.roots).unwrap();
        let entries = loader.entries(&fx.context()).unwrap();
        let arch = entries.iter().find(|e| e.title == "Arch Linux").unwrap();

        loader.set_cmdline(&fx.context(), arch, "root=UUID=abc rw loglevel=3").unwrap();

        let text = tree.read("EFI/refind/refind.conf");
        // Unquoted, the spaces would be read as separate directives.
        assert!(text.contains(r#"options "root=UUID=abc rw loglevel=3""#));

        let reread = loader.entries(&fx.context()).unwrap();
        assert_eq!(reread[0].cmdline, "root=UUID=abc rw loglevel=3");
        assert_eq!(reread.len(), 2);
    }
}
