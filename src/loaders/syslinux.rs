//! The Syslinux config family: extlinux, syslinux, isolinux and pxelinux.
//!
//! All four read the same directive language, so one adapter serves them all
//! and only the reported [`LoaderKind`] differs. extlinux is by far the most
//! important of the four here: it is the config U-Boot's distro boot path
//! reads, which makes it the standard way ARM boards boot Linux.
//!
//! Two details of the format are easy to get wrong and both are handled
//! explicitly below:
//!
//! - `TIMEOUT` is measured in **tenths of a second**, not seconds.
//! - `TIMEOUT 0` means *wait forever*, the opposite of GRUB's `0`.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::model::{BootEntry, EntryFlags, LoaderKind};
use crate::sys::atomic::{self, WriteOutcome};

use super::{resolve_under, scan::BootRoots, Bootloader, Capabilities, Context, Timeout};

/// One `LABEL` block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Label {
    pub name: String,
    /// `MENU LABEL`, the human-readable title when present.
    pub menu_label: Option<String>,
    pub kernel: Option<String>,
    pub initrds: Vec<String>,
    pub fdt: Option<String>,
    pub cmdline: String,
    /// This block carried `MENU DEFAULT`.
    pub menu_default: bool,
    /// Line index of the `APPEND` directive, for in-place edits.
    pub append_line: Option<usize>,
    /// Line index of the `LABEL` line, so an append can be inserted after it.
    pub label_line: usize,
}

impl Label {
    /// What to show the user: the menu label if the config supplies one,
    /// otherwise the raw label name.
    pub fn title(&self) -> String {
        self.menu_label.clone().unwrap_or_else(|| self.name.clone())
    }
}

#[derive(Debug, Clone, Default)]
pub struct SyslinuxConfig {
    /// `DEFAULT`, naming a label.
    pub default: Option<String>,
    /// `TIMEOUT`, in tenths of a second exactly as written.
    pub timeout_tenths: Option<u32>,
    pub labels: Vec<Label>,
    /// `INCLUDE` directives, which we surface for backup but do not follow.
    pub includes: Vec<String>,
}

/// Split a directive into its keyword (upper-cased) and the rest of the line.
fn split_directive(line: &str) -> Option<(String, &str)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    match trimmed.split_once(char::is_whitespace) {
        Some((k, v)) => Some((k.to_ascii_uppercase(), v.trim())),
        None => Some((trimmed.to_ascii_uppercase(), "")),
    }
}

/// Parse an extlinux/syslinux config.
pub fn parse(text: &str) -> SyslinuxConfig {
    let mut cfg = SyslinuxConfig::default();

    for (i, line) in text.lines().enumerate() {
        let Some((keyword, value)) = split_directive(line) else { continue };

        match keyword.as_str() {
            "LABEL" => {
                cfg.labels.push(Label {
                    name: value.to_string(),
                    label_line: i,
                    ..Default::default()
                });
                continue;
            }
            "DEFAULT" => {
                cfg.default = Some(value.to_string());
                continue;
            }
            "TIMEOUT" => {
                cfg.timeout_tenths = value.trim().parse().ok();
                continue;
            }
            // TOTALTIMEOUT caps the whole menu session rather than the wait
            // before the default boots, so it is not our timeout.
            "TOTALTIMEOUT" => continue,
            "INCLUDE" => {
                cfg.includes.push(value.to_string());
                continue;
            }
            _ => {}
        }

        // Everything else belongs to the label block currently open.
        let Some(label) = cfg.labels.last_mut() else { continue };

        match keyword.as_str() {
            // KERNEL and LINUX are interchangeable, and BOOT is the same
            // directive for boot-sector images.
            "KERNEL" | "LINUX" | "BOOT" => label.kernel = Some(value.to_string()),
            "INITRD" => {
                // One directive may list several images, comma-separated.
                label.initrds.extend(
                    value.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string),
                );
            }
            "FDT" | "DEVICETREE" => label.fdt = Some(value.to_string()),
            "APPEND" => {
                // `APPEND -` explicitly means an empty command line.
                let cmdline = if value.trim() == "-" { "" } else { value };
                label.cmdline = cmdline.to_string();
                label.append_line = Some(i);

                // An initrd is often passed as a kernel parameter instead of
                // its own directive, so pick it up from here too.
                for param in cmdline.split_whitespace() {
                    if let Some(v) = param.strip_prefix("initrd=") {
                        label
                            .initrds
                            .extend(v.split(',').filter(|s| !s.is_empty()).map(str::to_string));
                    }
                }
            }
            "MENU" => {
                let (sub, rest) = value
                    .split_once(char::is_whitespace)
                    .map(|(a, b)| (a.to_ascii_uppercase(), b.trim()))
                    .unwrap_or_else(|| (value.to_ascii_uppercase(), ""));
                match sub.as_str() {
                    "LABEL" => label.menu_label = Some(rest.to_string()),
                    "DEFAULT" => label.menu_default = true,
                    _ => {}
                }
            }
            _ => {}
        }
    }

    cfg
}

/// Convert a normalized timeout into syslinux's tenths-of-a-second value.
fn timeout_to_tenths(t: Timeout) -> u32 {
    match t {
        // Zero means "wait forever" here, so the fastest real menu is one
        // tenth of a second.
        Timeout::Immediate => 1,
        Timeout::Seconds(n) => n.saturating_mul(10),
        Timeout::Indefinite => 0,
    }
}

fn timeout_from_tenths(tenths: u32) -> Timeout {
    match tenths {
        0 => Timeout::Indefinite,
        // Anything under a second is effectively no menu at all.
        t if t < 10 => Timeout::Immediate,
        t => Timeout::Seconds(t / 10),
    }
}

pub struct Syslinux {
    config: PathBuf,
    boot_root: PathBuf,
    kind: LoaderKind,
    confidence: u8,
}

/// Config locations, paired with the loader they indicate.
const CANDIDATES: &[(&str, LoaderKind)] = &[
    ("extlinux/extlinux.conf", LoaderKind::Extlinux),
    ("extlinux.conf", LoaderKind::Extlinux),
    ("syslinux/syslinux.cfg", LoaderKind::Syslinux),
    ("syslinux.cfg", LoaderKind::Syslinux),
    ("isolinux/isolinux.cfg", LoaderKind::Syslinux),
    ("boot/syslinux/syslinux.cfg", LoaderKind::Syslinux),
    ("pxelinux.cfg/default", LoaderKind::Syslinux),
];

impl Syslinux {
    pub fn detect(roots: &BootRoots) -> Option<Syslinux> {
        for root in &roots.boot {
            for (name, kind) in CANDIDATES {
                let path = root.join(name);
                if !path.is_file() {
                    continue;
                }
                // On an ARM board, extlinux.conf plus a U-Boot script is the
                // strongest signal that this is how the machine actually boots.
                let uboot = root.join("boot.scr").exists() || root.join("boot.txt").exists();
                let confidence = match (kind, uboot) {
                    (LoaderKind::Extlinux, true) => 88,
                    (LoaderKind::Extlinux, false) => 78,
                    _ => 72,
                };
                return Some(Syslinux {
                    config: path,
                    boot_root: root.clone(),
                    kind: *kind,
                    confidence,
                });
            }
        }
        None
    }

    fn load(&self) -> Result<(SyslinuxConfig, String)> {
        let text = atomic::read_to_string(&self.config)?;
        Ok((parse(&text), text))
    }

    /// The directory the config lives in - extlinux resolves relative paths
    /// against it, rather than against the partition root.
    fn config_dir(&self) -> &Path {
        self.config.parent().unwrap_or(&self.boot_root)
    }

    /// Resolve a path from the config, trying the config directory first.
    fn resolve(&self, raw: &str) -> PathBuf {
        let dir_relative = self.config_dir().join(raw.trim_start_matches('/'));
        if dir_relative.exists() {
            return dir_relative;
        }
        resolve_under(&self.boot_root, raw)
    }
}

/// Replace a top-level directive's value, preserving the rest of the file.
fn set_directive(text: &str, keyword: &str, value: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut replaced = false;
    let mut seen_label = false;

    for line in text.lines() {
        let directive = split_directive(line);
        if directive.as_ref().is_some_and(|(k, _)| k == "LABEL") {
            seen_label = true;
        }
        // Only touch the directive while it is still in the global section;
        // the same keyword inside a LABEL block means something else.
        let is_target =
            !seen_label && directive.as_ref().is_some_and(|(k, _)| k == keyword);

        if is_target && !replaced {
            out.push(format!("{keyword} {value}"));
            replaced = true;
        } else if !is_target {
            out.push(line.to_string());
        }
    }

    if !replaced {
        // Globals must precede the first LABEL block.
        let at = out
            .iter()
            .position(|l| split_directive(l).is_some_and(|(k, _)| k == "LABEL"))
            .unwrap_or(out.len());
        out.insert(at, format!("{keyword} {value}"));
    }

    let mut joined = out.join("\n");
    joined.push('\n');
    joined
}

/// Replace (or insert) the `APPEND` line of one label.
fn set_label_append(text: &str, label: &Label, cmdline: &str) -> Result<String> {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();

    match label.append_line {
        Some(n) => {
            let indent: String =
                lines.get(n).map(|l| l.chars().take_while(|c| c.is_whitespace()).collect())
                    .unwrap_or_default();
            *lines
                .get_mut(n)
                .ok_or_else(|| Error::other("config changed on disk since it was read"))? =
                format!("{indent}APPEND {cmdline}");
        }
        None => {
            // No APPEND yet: put one directly after the LABEL line, indented to
            // match the block's other directives.
            let insert_at = (label.label_line + 1).min(lines.len());
            lines.insert(insert_at, format!("    APPEND {cmdline}"));
        }
    }

    let mut joined = lines.join("\n");
    joined.push('\n');
    Ok(joined)
}

impl Bootloader for Syslinux {
    fn kind(&self) -> LoaderKind {
        self.kind
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::SET_DEFAULT | Capabilities::TIMEOUT | Capabilities::EDIT_CMDLINE
    }

    fn confidence(&self) -> u8 {
        self.confidence
    }

    fn config_files(&self) -> Vec<PathBuf> {
        let mut files = vec![self.config.clone()];
        // U-Boot's compiled boot script sits alongside and is worth backing up
        // even though we cannot parse it.
        for extra in ["boot.scr", "boot.txt"] {
            let p = self.boot_root.join(extra);
            if p.exists() {
                files.push(p);
            }
        }
        files
    }

    fn entries(&self, _ctx: &Context) -> Result<Vec<BootEntry>> {
        let (cfg, _) = self.load()?;
        let default = cfg.default.as_deref().unwrap_or_default();

        Ok(cfg
            .labels
            .iter()
            .map(|label| {
                let mut entry =
                    BootEntry::new(self.kind, &self.config, &label.name, label.title());

                entry.kernel = label.kernel.as_deref().map(|p| self.resolve(p));
                entry.initrds = label.initrds.iter().map(|p| self.resolve(p)).collect();
                entry.devicetree = label.fdt.as_deref().map(|p| self.resolve(p));
                entry.cmdline = label.cmdline.clone();

                if label.menu_default || label.name == default {
                    entry.flags.insert(EntryFlags::DEFAULT);
                }
                if label.menu_label.is_some() {
                    entry.extra.insert("label".into(), label.name.clone());
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
        let updated = set_directive(&text, "DEFAULT", &entry.native_id);
        Ok(vec![atomic::write_atomic(&self.config, updated.as_bytes())?])
    }

    fn timeout(&self, _ctx: &Context) -> Result<Option<Timeout>> {
        let (cfg, _) = self.load()?;
        Ok(cfg.timeout_tenths.map(timeout_from_tenths))
    }

    fn set_timeout(&self, ctx: &Context, timeout: Timeout) -> Result<Vec<WriteOutcome>> {
        ctx.preflight_write("timeout", &self.config)?;
        if ctx.dry_run {
            return Ok(Vec::new());
        }
        let (_, text) = self.load()?;
        let updated =
            set_directive(&text, "TIMEOUT", &timeout_to_tenths(timeout).to_string());
        Ok(vec![atomic::write_atomic(&self.config, updated.as_bytes())?])
    }

    fn set_cmdline(&self, ctx: &Context, entry: &BootEntry, cmdline: &str) -> Result<Vec<WriteOutcome>> {
        ctx.preflight_write("cmdline set", &self.config)?;
        let (cfg, text) = self.load()?;
        let label = cfg
            .labels
            .iter()
            .find(|l| l.name == entry.native_id)
            .ok_or_else(|| Error::EntryNotFound { pattern: entry.native_id.clone() })?;

        if ctx.dry_run {
            return Ok(Vec::new());
        }
        let updated = set_label_append(&text, label, cmdline)?;
        Ok(vec![atomic::write_atomic(&self.config, updated.as_bytes())?])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loaders::testsupport::{fake_kernel, Fixture, TempTree};

    const CONF: &str = "\
# Generated by extlinux
DEFAULT linux
TIMEOUT 50
MENU TITLE Boot Menu

LABEL linux
    MENU LABEL Linux 6.11.0
    LINUX /Image
    FDT /dtbs/rk3399.dtb
    INITRD /initramfs-linux.img
    APPEND root=/dev/mmcblk0p2 rw rootwait console=ttyS2,1500000

LABEL rescue
    MENU LABEL Rescue shell
    KERNEL /Image
    APPEND root=/dev/mmcblk0p2 rw single
";

    #[test]
    fn parses_labels_and_globals() {
        let cfg = parse(CONF);
        assert_eq!(cfg.default.as_deref(), Some("linux"));
        assert_eq!(cfg.timeout_tenths, Some(50));
        assert_eq!(cfg.labels.len(), 2);

        let linux = &cfg.labels[0];
        assert_eq!(linux.name, "linux");
        assert_eq!(linux.title(), "Linux 6.11.0");
        assert_eq!(linux.kernel.as_deref(), Some("/Image"));
        assert_eq!(linux.initrds, vec!["/initramfs-linux.img"]);
        assert_eq!(linux.fdt.as_deref(), Some("/dtbs/rk3399.dtb"));
        assert!(linux.cmdline.contains("rootwait"));
    }

    #[test]
    fn falls_back_to_the_label_name_as_title() {
        let cfg = parse("LABEL bare\n    LINUX /vmlinuz\n");
        assert_eq!(cfg.labels[0].title(), "bare");
    }

    #[test]
    fn directives_are_case_insensitive() {
        let cfg = parse("default linux\ntimeout 30\nlabel linux\n    kernel /vmlinuz\n");
        assert_eq!(cfg.default.as_deref(), Some("linux"));
        assert_eq!(cfg.timeout_tenths, Some(30));
        assert_eq!(cfg.labels[0].kernel.as_deref(), Some("/vmlinuz"));
    }

    #[test]
    fn treats_append_dash_as_empty() {
        let cfg = parse("LABEL x\n    KERNEL /vmlinuz\n    APPEND -\n");
        assert_eq!(cfg.labels[0].cmdline, "");
    }

    #[test]
    fn picks_up_initrd_passed_as_a_kernel_parameter() {
        let cfg = parse("LABEL x\n    KERNEL /vmlinuz\n    APPEND initrd=/initrd.img root=/dev/sda1\n");
        assert_eq!(cfg.labels[0].initrds, vec!["/initrd.img"]);
    }

    #[test]
    fn splits_comma_separated_initrds() {
        let cfg = parse("LABEL x\n    KERNEL /vmlinuz\n    INITRD /ucode.img,/initramfs.img\n");
        assert_eq!(cfg.labels[0].initrds, vec!["/ucode.img", "/initramfs.img"]);
    }

    #[test]
    fn honours_menu_default() {
        let cfg = parse("LABEL a\n    KERNEL /a\nLABEL b\n    MENU DEFAULT\n    KERNEL /b\n");
        assert!(!cfg.labels[0].menu_default);
        assert!(cfg.labels[1].menu_default);
    }

    #[test]
    fn timeout_is_in_tenths_and_zero_means_forever() {
        // This convention is the opposite of GRUB's, where 0 boots at once.
        assert_eq!(timeout_from_tenths(0), Timeout::Indefinite);
        assert_eq!(timeout_from_tenths(50), Timeout::Seconds(5));
        assert_eq!(timeout_from_tenths(1), Timeout::Immediate);

        assert_eq!(timeout_to_tenths(Timeout::Seconds(5)), 50);
        assert_eq!(timeout_to_tenths(Timeout::Indefinite), 0);
        // Zero would mean "wait forever", so the closest real value is used.
        assert_eq!(timeout_to_tenths(Timeout::Immediate), 1);
    }

    #[test]
    fn timeout_round_trips() {
        for t in [Timeout::Immediate, Timeout::Seconds(12), Timeout::Indefinite] {
            assert_eq!(timeout_from_tenths(timeout_to_tenths(t)), t);
        }
    }

    #[test]
    fn sets_a_global_directive_without_touching_labels() {
        let out = set_directive(CONF, "DEFAULT", "rescue");
        assert!(out.contains("DEFAULT rescue"));
        assert!(!out.contains("DEFAULT linux"));
        let cfg = parse(&out);
        assert_eq!(cfg.default.as_deref(), Some("rescue"));
        assert_eq!(cfg.labels.len(), 2);
        assert_eq!(cfg.timeout_tenths, Some(50));
    }

    #[test]
    fn inserts_a_missing_global_before_the_first_label() {
        let text = "LABEL linux\n    KERNEL /vmlinuz\n";
        let out = set_directive(text, "TIMEOUT", "30");
        let cfg = parse(&out);
        // Placed after LABEL it would be read as part of that block.
        assert_eq!(cfg.timeout_tenths, Some(30));
        assert_eq!(cfg.labels.len(), 1);
    }

    #[test]
    fn edits_one_labels_append_only() {
        let cfg = parse(CONF);
        let out = set_label_append(CONF, &cfg.labels[0], "root=/dev/mmcblk0p2 rw debug").unwrap();
        let reparsed = parse(&out);
        assert_eq!(reparsed.labels[0].cmdline, "root=/dev/mmcblk0p2 rw debug");
        assert!(reparsed.labels[1].cmdline.contains("single"));
        // Other directives in the block survive.
        assert_eq!(reparsed.labels[0].fdt.as_deref(), Some("/dtbs/rk3399.dtb"));
    }

    #[test]
    fn inserts_append_for_a_label_without_one() {
        let text = "LABEL linux\n    KERNEL /vmlinuz\n";
        let cfg = parse(text);
        let out = set_label_append(text, &cfg.labels[0], "quiet").unwrap();
        let reparsed = parse(&out);
        assert_eq!(reparsed.labels[0].cmdline, "quiet");
        assert_eq!(reparsed.labels[0].kernel.as_deref(), Some("/vmlinuz"));
    }

    // ---- fixture-backed ---------------------------------------------

    fn extlinux_tree(tag: &str) -> TempTree {
        let tree = TempTree::new(tag);
        tree.file("extlinux/extlinux.conf", CONF);
        fake_kernel(&tree, "Image");
        fake_kernel(&tree, "initramfs-linux.img");
        fake_kernel(&tree, "dtbs/rk3399.dtb");
        tree
    }

    #[test]
    fn detects_extlinux() {
        let tree = extlinux_tree("extlinux-detect");
        let loader = Syslinux::detect(&tree.roots()).expect("extlinux detected");
        assert_eq!(loader.kind, LoaderKind::Extlinux);
        assert_eq!(loader.confidence, 78);
    }

    #[test]
    fn scores_higher_alongside_a_uboot_script() {
        let tree = extlinux_tree("extlinux-uboot");
        tree.file("boot.scr", "compiled u-boot script");
        let loader = Syslinux::detect(&tree.roots()).unwrap();
        assert_eq!(loader.confidence, 88);
        // The script is backed up even though it is not parsed.
        assert!(loader.config_files().contains(&tree.path("boot.scr")));
    }

    #[test]
    fn detects_syslinux_separately_from_extlinux() {
        let tree = TempTree::new("syslinux-detect");
        tree.file("syslinux/syslinux.cfg", CONF);
        let loader = Syslinux::detect(&tree.roots()).unwrap();
        assert_eq!(loader.kind, LoaderKind::Syslinux);
    }

    #[test]
    fn produces_normalized_entries_with_the_default_marked() {
        let tree = extlinux_tree("extlinux-entries");
        let fx = Fixture::rooted(tree.roots());
        let loader = Syslinux::detect(&fx.roots).unwrap();

        let entries = loader.entries(&fx.context()).unwrap();
        assert_eq!(entries.len(), 2);

        let linux = &entries[0];
        assert_eq!(linux.title, "Linux 6.11.0");
        assert_eq!(linux.native_id, "linux");
        assert!(linux.is_default());
        assert_eq!(linux.kernel.as_ref().unwrap(), &tree.path("Image"));
        assert_eq!(linux.devicetree.as_ref().unwrap(), &tree.path("dtbs/rk3399.dtb"));
        assert!(!entries[1].is_default());
    }

    #[test]
    fn set_default_switches_the_marked_entry() {
        let tree = extlinux_tree("extlinux-setdefault");
        let fx = Fixture::rooted(tree.roots());
        let loader = Syslinux::detect(&fx.roots).unwrap();
        let entries = loader.entries(&fx.context()).unwrap();
        let rescue = entries.iter().find(|e| e.native_id == "rescue").unwrap();

        loader.set_default(&fx.context(), rescue).unwrap();

        let reread = loader.entries(&fx.context()).unwrap();
        assert!(reread.iter().find(|e| e.native_id == "rescue").unwrap().is_default());
        assert!(!reread.iter().find(|e| e.native_id == "linux").unwrap().is_default());
    }

    #[test]
    fn timeout_round_trips_through_the_config() {
        let tree = extlinux_tree("extlinux-timeout");
        let fx = Fixture::rooted(tree.roots());
        let loader = Syslinux::detect(&fx.roots).unwrap();

        assert_eq!(loader.timeout(&fx.context()).unwrap(), Some(Timeout::Seconds(5)));

        loader.set_timeout(&fx.context(), Timeout::Seconds(12)).unwrap();
        assert!(tree.read("extlinux/extlinux.conf").contains("TIMEOUT 120"));
        assert_eq!(loader.timeout(&fx.context()).unwrap(), Some(Timeout::Seconds(12)));
    }

    #[test]
    fn set_cmdline_edits_the_right_label() {
        let tree = extlinux_tree("extlinux-cmdline");
        let fx = Fixture::rooted(tree.roots());
        let loader = Syslinux::detect(&fx.roots).unwrap();
        let entries = loader.entries(&fx.context()).unwrap();
        let rescue = entries.iter().find(|e| e.native_id == "rescue").unwrap();

        loader.set_cmdline(&fx.context(), rescue, "root=/dev/mmcblk0p2 rw emergency").unwrap();

        let reread = loader.entries(&fx.context()).unwrap();
        assert_eq!(
            reread.iter().find(|e| e.native_id == "rescue").unwrap().cmdline,
            "root=/dev/mmcblk0p2 rw emergency"
        );
        assert!(reread.iter().find(|e| e.native_id == "linux").unwrap().cmdline.contains("rootwait"));
    }
}
