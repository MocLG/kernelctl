//! LILO.
//!
//! LILO is unlike every other loader here in one crucial respect: it has no
//! boot-time config reader. `/etc/lilo.conf` is input to the `lilo` command,
//! which stamps a block map into the boot sector. **Editing the file changes
//! nothing until `lilo` is run again**, so every write reports that clearly
//! rather than letting the user believe a reboot will pick it up.
//!
//! One-shot boot is the exception: `lilo -R <label>` writes a single-use
//! default directly into the boot sector, so it takes effect immediately and
//! is implemented by invoking the tool.

use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::model::{BootEntry, EntryFlags, LoaderKind};
use crate::sys::atomic::{self, WriteOutcome};
use crate::sys::exec;

use super::{scan::BootRoots, Bootloader, Capabilities, Context, Timeout};

const CONFIG: &str = "/etc/lilo.conf";

/// One `image=` or `other=` stanza.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiloImage {
    pub label: String,
    /// Kernel path for `image=`, or the partition for `other=`.
    pub target: String,
    pub initrd: Option<String>,
    pub append: String,
    pub root: Option<String>,
    /// `other=` stanzas chainload another OS rather than a kernel.
    pub is_other: bool,
    pub line: usize,
    pub append_line: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct LiloConfig {
    pub default: Option<String>,
    /// `timeout` in tenths of a second, as written.
    pub timeout_tenths: Option<u32>,
    pub boot_device: Option<String>,
    /// Global `append`, inherited by images without their own.
    pub global_append: Option<String>,
    pub images: Vec<LiloImage>,
}

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

/// Split a `key=value` line, or return a bare flag with an empty value.
fn split_setting(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    match trimmed.split_once('=') {
        Some((k, v)) => Some((k.trim().to_ascii_lowercase(), unquote(v))),
        None => Some((trimmed.to_ascii_lowercase(), String::new())),
    }
}

/// Parse /etc/lilo.conf.
pub fn parse(text: &str) -> LiloConfig {
    let mut cfg = LiloConfig::default();

    for (i, line) in text.lines().enumerate() {
        let Some((key, value)) = split_setting(line) else { continue };

        match key.as_str() {
            // A new stanza starts here and everything below belongs to it.
            "image" | "other" => {
                cfg.images.push(LiloImage {
                    target: value,
                    is_other: key == "other",
                    line: i,
                    ..Default::default()
                });
                continue;
            }
            "default" => {
                cfg.default = Some(value);
                continue;
            }
            "timeout" => {
                cfg.timeout_tenths = value.trim().parse().ok();
                continue;
            }
            "boot" if cfg.images.is_empty() => {
                cfg.boot_device = Some(value);
                continue;
            }
            "append" if cfg.images.is_empty() => {
                cfg.global_append = Some(value);
                continue;
            }
            _ => {}
        }

        let Some(image) = cfg.images.last_mut() else { continue };
        match key.as_str() {
            "label" => image.label = value,
            "initrd" | "ramdisk" => image.initrd = Some(value),
            "append" => {
                image.append = value;
                image.append_line = Some(i);
            }
            "root" => image.root = Some(value),
            _ => {}
        }
    }

    cfg
}

impl LiloConfig {
    /// The full command line for an image: its own `append`, plus the global
    /// one it inherits, plus the `root=` LILO passes separately.
    pub fn cmdline_for(&self, image: &LiloImage) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(root) = &image.root {
            parts.push(format!("root={root}"));
        }
        if let Some(global) = &self.global_append {
            if !global.trim().is_empty() {
                parts.push(global.clone());
            }
        }
        if !image.append.trim().is_empty() {
            parts.push(image.append.clone());
        }
        parts.join(" ")
    }
}

pub struct Lilo {
    config: PathBuf,
}

impl Lilo {
    pub fn detect(_roots: &BootRoots) -> Option<Lilo> {
        let path = PathBuf::from(CONFIG);
        path.is_file().then(|| Lilo { config: path })
    }

    fn load(&self) -> Result<(LiloConfig, String)> {
        let text = atomic::read_to_string(&self.config)?;
        Ok((parse(&text), text))
    }
}

/// Replace a global `key=value`, keeping it above the first stanza.
fn set_global(text: &str, key: &str, value: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut replaced = false;
    let mut first_stanza: Option<usize> = None;

    for line in text.lines() {
        let setting = split_setting(line);
        if first_stanza.is_none()
            && setting.as_ref().is_some_and(|(k, _)| k == "image" || k == "other")
        {
            first_stanza = Some(out.len());
        }
        let is_target =
            first_stanza.is_none() && setting.as_ref().is_some_and(|(k, _)| k == key);

        if is_target && !replaced {
            out.push(format!("{key}={value}"));
            replaced = true;
        } else if !is_target {
            out.push(line.to_string());
        }
    }

    if !replaced {
        // Below a stanza it would be read as part of that image.
        out.insert(first_stanza.unwrap_or(out.len()), format!("{key}={value}"));
    }

    let mut joined = out.join("\n");
    joined.push('\n');
    joined
}

impl Bootloader for Lilo {
    fn kind(&self) -> LoaderKind {
        LoaderKind::Lilo
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::SET_DEFAULT
            | Capabilities::SET_ONESHOT
            | Capabilities::TIMEOUT
            | Capabilities::EDIT_CMDLINE
    }

    fn confidence(&self) -> u8 {
        // LILO has been superseded nearly everywhere, so a lilo.conf left on
        // disk is more often a leftover than the active loader.
        45
    }

    fn config_files(&self) -> Vec<PathBuf> {
        vec![self.config.clone()]
    }

    fn post_write_note(&self) -> Option<String> {
        Some(
            "LILO reads no config at boot: run `lilo` as root to write the change into \
             the boot sector, or the next boot will use the previous configuration"
                .to_string(),
        )
    }

    fn entries(&self, _ctx: &Context) -> Result<Vec<BootEntry>> {
        let (cfg, _) = self.load()?;
        let default = cfg.default.clone().unwrap_or_default();

        Ok(cfg
            .images
            .iter()
            .map(|image| {
                // A stanza without a label is addressed by its path, which is
                // what LILO itself falls back to.
                let label =
                    if image.label.is_empty() { image.target.clone() } else { image.label.clone() };

                let mut entry =
                    BootEntry::new(LoaderKind::Lilo, &self.config, &label, &label);

                if image.is_other {
                    entry.flags.insert(EntryFlags::CHAINLOAD);
                    entry.extra.insert("device".into(), image.target.clone());
                } else {
                    entry.kernel = Some(PathBuf::from(&image.target));
                    entry.initrds =
                        image.initrd.iter().map(PathBuf::from).collect();
                    entry.cmdline = cfg.cmdline_for(image);
                }

                if !default.is_empty() && label == default {
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
        let updated = set_global(&text, "default", &entry.native_id);
        Ok(vec![atomic::write_atomic(&self.config, updated.as_bytes())?])
    }

    fn set_oneshot(&self, ctx: &Context, entry: &BootEntry) -> Result<Vec<WriteOutcome>> {
        ctx.privileges.require("set-next")?;
        // `lilo -R` is the only way to reach the boot sector's one-shot slot;
        // there is no file to edit.
        exec::require(
            "lilo",
            "install the lilo package, or use `kernelctl set-default` instead",
        )?;
        if ctx.dry_run {
            return Ok(Vec::new());
        }
        exec::run("lilo", ["-R", &entry.native_id])?;
        Ok(Vec::new())
    }

    fn clear_oneshot(&self, ctx: &Context) -> Result<Vec<WriteOutcome>> {
        ctx.privileges.require("set-next --clear")?;
        exec::require("lilo", "install the lilo package")?;
        if ctx.dry_run {
            return Ok(Vec::new());
        }
        // `-R` with no label clears the pending one-shot.
        exec::run("lilo", ["-R"])?;
        Ok(Vec::new())
    }

    fn timeout(&self, _ctx: &Context) -> Result<Option<Timeout>> {
        let (cfg, _) = self.load()?;
        // LILO's timeout is in tenths of a second, and 0 disables the prompt.
        Ok(cfg.timeout_tenths.map(|t| match t {
            0 => Timeout::Immediate,
            t if t < 10 => Timeout::Immediate,
            t => Timeout::Seconds(t / 10),
        }))
    }

    fn set_timeout(&self, ctx: &Context, timeout: Timeout) -> Result<Vec<WriteOutcome>> {
        ctx.preflight_write("timeout", &self.config)?;
        if ctx.dry_run {
            return Ok(Vec::new());
        }
        let value = match timeout {
            Timeout::Immediate => "0".to_string(),
            Timeout::Seconds(n) => (n.saturating_mul(10)).to_string(),
            Timeout::Indefinite => {
                // LILO has no "wait forever" timeout; `prompt` without a
                // timeout is the equivalent, which is a different directive.
                return Err(Error::unsupported(
                    "LILO",
                    "an indefinite timeout (remove the `timeout` line and keep `prompt` instead)",
                ));
            }
        };
        let (_, text) = self.load()?;
        let updated = set_global(&text, "timeout", &value);
        Ok(vec![atomic::write_atomic(&self.config, updated.as_bytes())?])
    }

    fn set_cmdline(&self, ctx: &Context, entry: &BootEntry, cmdline: &str) -> Result<Vec<WriteOutcome>> {
        ctx.preflight_write("cmdline set", &self.config)?;
        let (cfg, text) = self.load()?;
        let image = cfg
            .images
            .iter()
            .find(|i| i.label == entry.native_id || i.target == entry.native_id)
            .ok_or_else(|| Error::EntryNotFound { pattern: entry.native_id.clone() })?;

        if ctx.dry_run {
            return Ok(Vec::new());
        }

        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        let quoted = format!("\"{}\"", cmdline.replace('"', "\\\""));

        match image.append_line {
            Some(n) => {
                let indent: String = lines[n].chars().take_while(|c| c.is_whitespace()).collect();
                lines[n] = format!("{indent}append={quoted}");
            }
            None => lines.insert((image.line + 1).min(lines.len()), format!("\tappend={quoted}")),
        }

        let mut joined = lines.join("\n");
        joined.push('\n');
        Ok(vec![atomic::write_atomic(&self.config, joined.as_bytes())?])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONF: &str = r#"
boot=/dev/sda
prompt
timeout=50
default=Linux
append="quiet"

image=/boot/vmlinuz-6.11.0
	label=Linux
	initrd=/boot/initrd.img-6.11.0
	root=/dev/sda1
	append="ro splash"
	read-only

image=/boot/vmlinuz-6.10.0
	label=Linux-old
	initrd=/boot/initrd.img-6.10.0
	root=/dev/sda1

other=/dev/sda2
	label=Windows
"#;

    #[test]
    fn parses_images_and_globals() {
        let cfg = parse(CONF);
        assert_eq!(cfg.boot_device.as_deref(), Some("/dev/sda"));
        assert_eq!(cfg.default.as_deref(), Some("Linux"));
        assert_eq!(cfg.timeout_tenths, Some(50));
        assert_eq!(cfg.global_append.as_deref(), Some("quiet"));
        assert_eq!(cfg.images.len(), 3);

        let linux = &cfg.images[0];
        assert_eq!(linux.label, "Linux");
        assert_eq!(linux.target, "/boot/vmlinuz-6.11.0");
        assert_eq!(linux.initrd.as_deref(), Some("/boot/initrd.img-6.11.0"));
        assert_eq!(linux.append, "ro splash");
    }

    #[test]
    fn distinguishes_other_stanzas_as_chainloads() {
        let cfg = parse(CONF);
        assert!(cfg.images[2].is_other);
        assert_eq!(cfg.images[2].label, "Windows");
        assert!(!cfg.images[0].is_other);
    }

    #[test]
    fn composes_the_full_command_line() {
        let cfg = parse(CONF);
        // root=, then the inherited global append, then the image's own.
        assert_eq!(cfg.cmdline_for(&cfg.images[0]), "root=/dev/sda1 quiet ro splash");
        // An image without its own append still inherits the global one.
        assert_eq!(cfg.cmdline_for(&cfg.images[1]), "root=/dev/sda1 quiet");
    }

    #[test]
    fn a_global_append_only_counts_before_the_first_image() {
        // The second `append` belongs to the image, not to the file.
        let cfg = parse("append=\"global\"\nimage=/vmlinuz\n\tlabel=L\n\tappend=\"local\"\n");
        assert_eq!(cfg.global_append.as_deref(), Some("global"));
        assert_eq!(cfg.images[0].append, "local");
    }

    #[test]
    fn sets_a_global_above_the_stanzas() {
        let out = set_global(CONF, "default", "Windows");
        let cfg = parse(&out);
        assert_eq!(cfg.default.as_deref(), Some("Windows"));
        assert_eq!(cfg.images.len(), 3);
        assert_eq!(cfg.timeout_tenths, Some(50));
    }

    #[test]
    fn inserts_a_missing_global_before_the_first_stanza() {
        let text = "boot=/dev/sda\nimage=/vmlinuz\n\tlabel=L\n";
        let out = set_global(text, "default", "L");
        let cfg = parse(&out);
        // Below the stanza it would be swallowed by the image block.
        assert_eq!(cfg.default.as_deref(), Some("L"));
        assert_eq!(cfg.images.len(), 1);
    }

    // ---- adapter behaviour ---------------------------------------------

    use crate::loaders::testsupport::{Fixture, TempTree};

    /// Point the adapter at a scratch config rather than the real /etc.
    fn scratch(tag: &str) -> (TempTree, Lilo) {
        let tree = TempTree::new(tag);
        let path = tree.file("lilo.conf", CONF);
        (tree, Lilo { config: path })
    }

    #[test]
    fn produces_entries_with_the_default_marked() {
        let (_tree, loader) = scratch("lilo-entries");
        let fx = Fixture::rooted(BootRoots::default());

        let entries = loader.entries(&fx.context()).unwrap();
        assert_eq!(entries.len(), 3);
        assert!(entries[0].is_default());
        assert_eq!(entries[0].cmdline, "root=/dev/sda1 quiet ro splash");
        assert!(entries[2].flags.contains(EntryFlags::CHAINLOAD));
        assert!(entries[2].kernel.is_none());
    }

    #[test]
    fn always_warns_that_lilo_must_be_rerun() {
        let (_tree, loader) = scratch("lilo-note");
        // Editing the file alone changes nothing at boot, so this must be said.
        assert!(loader.post_write_note().unwrap().contains("run `lilo`"));
    }

    #[test]
    fn set_default_round_trips() {
        let (tree, loader) = scratch("lilo-setdefault");
        let fx = Fixture::rooted(BootRoots::default());
        let entries = loader.entries(&fx.context()).unwrap();
        let old = entries.iter().find(|e| e.native_id == "Linux-old").unwrap();

        loader.set_default(&fx.context(), old).unwrap();

        assert!(tree.read("lilo.conf").contains("default=Linux-old"));
        let reread = loader.entries(&fx.context()).unwrap();
        assert!(reread.iter().find(|e| e.native_id == "Linux-old").unwrap().is_default());
    }

    #[test]
    fn timeout_is_read_in_tenths() {
        let (_tree, loader) = scratch("lilo-timeout");
        let fx = Fixture::rooted(BootRoots::default());
        assert_eq!(loader.timeout(&fx.context()).unwrap(), Some(Timeout::Seconds(5)));
    }

    #[test]
    fn indefinite_timeout_is_reported_unsupported() {
        let (_tree, loader) = scratch("lilo-indefinite");
        let fx = Fixture::rooted(BootRoots::default());
        // LILO has no such value; saying so beats writing something wrong.
        let err = loader.set_timeout(&fx.context(), Timeout::Indefinite).unwrap_err();
        assert!(matches!(err, Error::Unsupported { .. }));
    }

    #[test]
    fn set_cmdline_edits_the_image_append() {
        let (tree, loader) = scratch("lilo-cmdline");
        let fx = Fixture::rooted(BootRoots::default());
        let entries = loader.entries(&fx.context()).unwrap();
        let linux = entries.iter().find(|e| e.native_id == "Linux").unwrap();

        loader.set_cmdline(&fx.context(), linux, "ro debug").unwrap();

        let text = tree.read("lilo.conf");
        assert!(text.contains(r#"append="ro debug""#));
        assert!(text.contains("label=Linux-old"), "other stanzas survive");

        let reread = loader.entries(&fx.context()).unwrap();
        assert_eq!(reread[0].cmdline, "root=/dev/sda1 quiet ro debug");
    }

    #[test]
    fn adds_an_append_to_a_stanza_without_one() {
        let (tree, loader) = scratch("lilo-addappend");
        let fx = Fixture::rooted(BootRoots::default());
        let entries = loader.entries(&fx.context()).unwrap();
        let old = entries.iter().find(|e| e.native_id == "Linux-old").unwrap();

        loader.set_cmdline(&fx.context(), old, "ro nomodeset").unwrap();

        let cfg = parse(&tree.read("lilo.conf"));
        assert_eq!(cfg.images[1].append, "ro nomodeset");
        assert_eq!(cfg.images[0].append, "ro splash");
    }
}
