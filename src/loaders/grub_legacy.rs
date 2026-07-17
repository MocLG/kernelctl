//! GRUB Legacy (0.9x), configured by `menu.lst`.
//!
//! A much simpler format than GRUB 2: a flat list of `title` blocks with no
//! nesting and no generated-config indirection, so `menu.lst` is edited
//! directly and the change is live at the next boot.
//!
//! `default` is a zero-based index rather than a name, which makes it fragile:
//! inserting an entry above the default silently changes which one boots. The
//! adapter writes the index but records the title alongside it in a comment so
//! the intent is recoverable.

use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::model::{BootEntry, EntryFlags, LoaderKind};
use crate::sys::atomic::{self, WriteOutcome};

use super::{resolve_under, scan::BootRoots, Bootloader, Capabilities, Context, Timeout};

/// One `title` block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LegacyEntry {
    pub title: String,
    pub kernel: Option<String>,
    pub cmdline: String,
    pub initrds: Vec<String>,
    pub root: Option<String>,
    /// Line index of the `kernel` directive, for in-place edits.
    pub kernel_line: Option<usize>,
    pub title_line: usize,
}

#[derive(Debug, Clone, Default)]
pub struct LegacyConfig {
    /// `default`, a zero-based index or the word `saved`.
    pub default: Option<String>,
    pub timeout: Option<u32>,
    pub fallback: Option<String>,
    pub entries: Vec<LegacyEntry>,
}

fn split_directive(line: &str) -> Option<(String, &str)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    match trimmed.split_once(char::is_whitespace) {
        Some((k, v)) => Some((k.to_ascii_lowercase(), v.trim())),
        None => Some((trimmed.to_ascii_lowercase(), "")),
    }
}

/// Parse a menu.lst.
pub fn parse(text: &str) -> LegacyConfig {
    let mut cfg = LegacyConfig::default();

    for (i, line) in text.lines().enumerate() {
        let Some((keyword, value)) = split_directive(line) else { continue };

        if keyword == "title" {
            cfg.entries.push(LegacyEntry {
                title: value.to_string(),
                title_line: i,
                ..Default::default()
            });
            continue;
        }

        // Directives before the first title are global.
        let Some(entry) = cfg.entries.last_mut() else {
            match keyword.as_str() {
                "default" => cfg.default = Some(value.to_string()),
                "timeout" => cfg.timeout = value.parse().ok(),
                "fallback" => cfg.fallback = Some(value.to_string()),
                _ => {}
            }
            continue;
        };

        match keyword.as_str() {
            "kernel" => {
                // Path first, command line after.
                let mut parts = value.splitn(2, char::is_whitespace);
                entry.kernel = parts.next().map(str::to_string);
                entry.cmdline = parts.next().unwrap_or("").trim().to_string();
                entry.kernel_line = Some(i);
            }
            // `module` is how Xen and multiboot kernels list their initrd.
            "initrd" | "module" => entry.initrds.push(value.to_string()),
            "root" | "uuid" => entry.root = Some(value.to_string()),
            _ => {}
        }
    }

    cfg
}

pub struct GrubLegacy {
    config: PathBuf,
    boot_root: PathBuf,
}

impl GrubLegacy {
    const CANDIDATES: [&'static str; 4] =
        ["grub/menu.lst", "boot/grub/menu.lst", "grub/grub.conf", "menu.lst"];

    pub fn detect(roots: &BootRoots) -> Option<GrubLegacy> {
        for root in &roots.boot {
            for name in Self::CANDIDATES {
                let path = root.join(name);
                // GRUB 2 installs a menu.lst symlink for compatibility on some
                // systems, so a directory holding grub.cfg is GRUB 2's, not
                // this loader's.
                if path.is_file() && !root.join("grub/grub.cfg").exists() {
                    return Some(GrubLegacy { config: path, boot_root: root.clone() });
                }
            }
        }
        None
    }

    fn load(&self) -> Result<(LegacyConfig, String)> {
        let text = atomic::read_to_string(&self.config)?;
        Ok((parse(&text), text))
    }
}

/// Replace a global directive, keeping it above the first title block.
fn set_global(text: &str, keyword: &str, value: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut replaced = false;
    let mut first_title: Option<usize> = None;

    for line in text.lines() {
        let directive = split_directive(line);
        if first_title.is_none() && directive.as_ref().is_some_and(|(k, _)| k == "title") {
            first_title = Some(out.len());
        }
        let is_target =
            first_title.is_none() && directive.as_ref().is_some_and(|(k, _)| k == keyword);

        if is_target && !replaced {
            out.push(format!("{keyword} {value}"));
            replaced = true;
        } else if !is_target {
            out.push(line.to_string());
        }
    }

    if !replaced {
        out.insert(first_title.unwrap_or(out.len()), format!("{keyword} {value}"));
    }

    let mut joined = out.join("\n");
    joined.push('\n');
    joined
}

impl Bootloader for GrubLegacy {
    fn kind(&self) -> LoaderKind {
        LoaderKind::GrubLegacy
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::SET_DEFAULT | Capabilities::TIMEOUT | Capabilities::EDIT_CMDLINE
    }

    fn confidence(&self) -> u8 {
        // Long superseded, so a menu.lst is usually a leftover.
        40
    }

    fn config_files(&self) -> Vec<PathBuf> {
        vec![self.config.clone()]
    }

    fn post_write_note(&self) -> Option<String> {
        Some(
            "GRUB Legacy selects the default by position, so adding or removing an entry \
             above it changes which one boots"
                .to_string(),
        )
    }

    fn entries(&self, _ctx: &Context) -> Result<Vec<BootEntry>> {
        let (cfg, _) = self.load()?;
        let default_index = cfg.default.as_deref().and_then(|d| d.parse::<usize>().ok());

        Ok(cfg
            .entries
            .iter()
            .enumerate()
            .map(|(i, le)| {
                // Index is the loader's own addressing, so it is the native id.
                let mut entry =
                    BootEntry::new(LoaderKind::GrubLegacy, &self.config, i.to_string(), &le.title);

                entry.kernel = le.kernel.as_deref().map(|p| resolve_under(&self.boot_root, p));
                entry.initrds =
                    le.initrds.iter().map(|p| resolve_under(&self.boot_root, p)).collect();
                entry.cmdline = le.cmdline.clone();

                if let Some(root) = &le.root {
                    entry.extra.insert("root".into(), root.clone());
                }
                if default_index == Some(i) {
                    entry.flags.insert(EntryFlags::DEFAULT);
                }
                if le.kernel.is_none() {
                    entry.flags.insert(EntryFlags::CHAINLOAD);
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
        // Just the bare index: GRUB Legacy only honours '#' at the start of a
        // line, so an explanatory trailing comment would be parsed as part of
        // the value and break the directive. The positional fragility is
        // reported through post_write_note instead.
        let updated = set_global(&text, "default", &entry.native_id);
        Ok(vec![atomic::write_atomic(&self.config, updated.as_bytes())?])
    }

    fn timeout(&self, _ctx: &Context) -> Result<Option<Timeout>> {
        let (cfg, _) = self.load()?;
        Ok(cfg.timeout.map(|t| if t == 0 { Timeout::Immediate } else { Timeout::Seconds(t) }))
    }

    fn set_timeout(&self, ctx: &Context, timeout: Timeout) -> Result<Vec<WriteOutcome>> {
        ctx.preflight_write("timeout", &self.config)?;
        if ctx.dry_run {
            return Ok(Vec::new());
        }
        let value = match timeout {
            Timeout::Immediate => "0".to_string(),
            Timeout::Seconds(n) => n.to_string(),
            Timeout::Indefinite => {
                return Err(Error::unsupported(
                    "GRUB Legacy",
                    "an indefinite timeout (omit the `timeout` line instead)",
                ))
            }
        };
        let (_, text) = self.load()?;
        let updated = set_global(&text, "timeout", &value);
        Ok(vec![atomic::write_atomic(&self.config, updated.as_bytes())?])
    }

    fn set_cmdline(&self, ctx: &Context, entry: &BootEntry, cmdline: &str) -> Result<Vec<WriteOutcome>> {
        ctx.preflight_write("cmdline set", &self.config)?;
        let (cfg, text) = self.load()?;
        let index: usize = entry
            .native_id
            .parse()
            .map_err(|_| Error::EntryNotFound { pattern: entry.native_id.clone() })?;
        let target = cfg
            .entries
            .get(index)
            .ok_or_else(|| Error::EntryNotFound { pattern: entry.native_id.clone() })?;
        let line_no = target
            .kernel_line
            .ok_or_else(|| Error::validation("this entry has no kernel line to edit"))?;

        if ctx.dry_run {
            return Ok(Vec::new());
        }

        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        let indent: String = lines[line_no].chars().take_while(|c| c.is_whitespace()).collect();
        let kernel = target.kernel.clone().unwrap_or_default();
        lines[line_no] = format!("{indent}kernel {kernel} {cmdline}").trim_end().to_string();

        let mut joined = lines.join("\n");
        joined.push('\n');
        Ok(vec![atomic::write_atomic(&self.config, joined.as_bytes())?])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loaders::testsupport::{fake_kernel, Fixture, TempTree};

    const MENU: &str = "\
# menu.lst
default 1
timeout 5
fallback 0

title Debian GNU/Linux, kernel 2.6.32-5-686
root (hd0,0)
kernel /boot/vmlinuz-2.6.32-5-686 root=/dev/sda1 ro quiet
initrd /boot/initrd.img-2.6.32-5-686

title Debian GNU/Linux, kernel 2.6.32-5-686 (single-user mode)
root (hd0,0)
kernel /boot/vmlinuz-2.6.32-5-686 root=/dev/sda1 ro single
initrd /boot/initrd.img-2.6.32-5-686

title Windows
rootnoverify (hd0,1)
chainloader +1
";

    #[test]
    fn parses_globals_and_entries() {
        let cfg = parse(MENU);
        assert_eq!(cfg.default.as_deref(), Some("1"));
        assert_eq!(cfg.timeout, Some(5));
        assert_eq!(cfg.fallback.as_deref(), Some("0"));
        assert_eq!(cfg.entries.len(), 3);

        let first = &cfg.entries[0];
        assert_eq!(first.kernel.as_deref(), Some("/boot/vmlinuz-2.6.32-5-686"));
        assert_eq!(first.cmdline, "root=/dev/sda1 ro quiet");
        assert_eq!(first.initrds, vec!["/boot/initrd.img-2.6.32-5-686"]);
        assert_eq!(first.root.as_deref(), Some("(hd0,0)"));
    }

    #[test]
    fn an_entry_without_a_kernel_is_a_chainload() {
        let cfg = parse(MENU);
        assert!(cfg.entries[2].kernel.is_none());
    }

    #[test]
    fn sets_a_global_above_the_first_title() {
        let out = set_global(MENU, "default", "2");
        let cfg = parse(&out);
        assert_eq!(cfg.default.as_deref(), Some("2"));
        assert_eq!(cfg.entries.len(), 3);
    }

    fn legacy_tree(tag: &str) -> (TempTree, GrubLegacy) {
        let tree = TempTree::new(tag);
        let path = tree.file("grub/menu.lst", MENU);
        fake_kernel(&tree, "boot/vmlinuz-2.6.32-5-686");
        let boot_root = tree.root.clone();
        (tree, GrubLegacy { config: path, boot_root })
    }

    #[test]
    fn detects_menu_lst_but_not_alongside_grub2() {
        let tree = TempTree::new("legacy-detect");
        tree.file("grub/menu.lst", MENU);
        assert!(GrubLegacy::detect(&tree.roots()).is_some());

        // GRUB 2 ships a compatibility menu.lst on some systems; a grub.cfg
        // next to it means GRUB 2 owns this directory.
        tree.file("grub/grub.cfg", "menuentry 'x' {}\n");
        assert!(GrubLegacy::detect(&tree.roots()).is_none());
    }

    #[test]
    fn marks_the_default_by_index() {
        let (_tree, loader) = legacy_tree("legacy-default");
        let fx = Fixture::rooted(BootRoots::default());
        let entries = loader.entries(&fx.context()).unwrap();

        // `default 1` is zero-based, so it selects the second entry.
        assert!(!entries[0].is_default());
        assert!(entries[1].is_default());
        assert!(entries[2].flags.contains(EntryFlags::CHAINLOAD));
    }

    #[test]
    fn set_default_writes_a_bare_index_that_reparses() {
        let (tree, loader) = legacy_tree("legacy-setdefault");
        let fx = Fixture::rooted(BootRoots::default());
        let entries = loader.entries(&fx.context()).unwrap();

        loader.set_default(&fx.context(), &entries[2]).unwrap();

        // GRUB Legacy only honours '#' at line start, so the value must carry
        // nothing but the index or the directive breaks.
        assert_eq!(
            tree.read("grub/menu.lst").lines().find(|l| l.starts_with("default")),
            Some("default 2")
        );

        let reread = loader.entries(&fx.context()).unwrap();
        assert!(reread[2].is_default());
        assert!(!reread[1].is_default());
    }

    #[test]
    fn set_cmdline_edits_the_matching_kernel_line() {
        let (tree, loader) = legacy_tree("legacy-cmdline");
        let fx = Fixture::rooted(BootRoots::default());
        let entries = loader.entries(&fx.context()).unwrap();

        loader.set_cmdline(&fx.context(), &entries[0], "root=/dev/sda1 ro debug").unwrap();

        let cfg = parse(&tree.read("grub/menu.lst"));
        assert_eq!(cfg.entries[0].cmdline, "root=/dev/sda1 ro debug");
        assert_eq!(cfg.entries[1].cmdline, "root=/dev/sda1 ro single");
        assert_eq!(cfg.entries[0].kernel.as_deref(), Some("/boot/vmlinuz-2.6.32-5-686"));
    }

    #[test]
    fn indefinite_timeout_is_reported_unsupported() {
        let (_tree, loader) = legacy_tree("legacy-timeout");
        let fx = Fixture::rooted(BootRoots::default());
        assert_eq!(loader.timeout(&fx.context()).unwrap(), Some(Timeout::Seconds(5)));
        assert!(matches!(
            loader.set_timeout(&fx.context(), Timeout::Indefinite).unwrap_err(),
            Error::Unsupported { .. }
        ));
    }
}
