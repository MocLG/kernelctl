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
//! Barebox.
//!
//! Barebox boots from two different kinds of source and reads both:
//!
//! - **Boot entries**: shell scripts under `/env/boot/`, each setting the
//!   `global.bootm.*` variables for one target. These are programs, not config
//!   files, so only the straightforward variable assignments are read - which
//!   covers how the scripts are conventionally written, but a script that
//!   computes its paths cannot be understood and is reported by name alone.
//! - **Bootloader Spec entries**: the same `loader/entries/*.conf` format
//!   systemd-boot uses. Barebox is more permissive than the spec and accepts
//!   them on any readable partition.
//!
//! The default is `global.boot.default`, set in `/env/nv/boot.default`.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::model::{BootEntry, EntryFlags, LoaderKind};
use crate::sys::atomic::{self, WriteOutcome};

use super::{bls, scan::BootRoots, Bootloader, Capabilities, Context};

/// Variables a boot script sets, as far as we can read them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BootScript {
    pub name: String,
    pub kernel: Option<String>,
    pub initrd: Option<String>,
    pub devicetree: Option<String>,
    /// Concatenation of the `linux.bootargs.*` assignments.
    pub bootargs: Vec<String>,
    /// True when the script does more than plain assignments, so what we read
    /// may be incomplete.
    pub has_logic: bool,
}

/// Strip surrounding quotes from a shell value.
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

/// Read what can be read from a Barebox boot script.
pub fn parse_script(name: &str, text: &str) -> BootScript {
    let mut script = BootScript { name: name.to_string(), ..Default::default() };

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Anything beyond a plain assignment means the real values may be
        // computed at boot, so flag that what we report is partial.
        if ["if ", "for ", "while ", "$(", "`"].iter().any(|kw| trimmed.starts_with(kw))
            || trimmed.contains("$(")
        {
            script.has_logic = true;
        }

        let Some((key, value)) = trimmed.split_once('=') else { continue };
        // A key with spaces is a command, not an assignment.
        if key.contains(char::is_whitespace) {
            continue;
        }
        let value = unquote(value);

        match key {
            "global.bootm.image" => script.kernel = Some(value),
            "global.bootm.initrd" => script.initrd = Some(value),
            "global.bootm.oftree" => script.devicetree = Some(value),
            k if k.starts_with("global.linux.bootargs.")
                || k.starts_with("linux.bootargs.") =>
            {
                if !value.trim().is_empty() {
                    script.bootargs.push(value);
                }
            }
            _ => {}
        }
    }

    script
}

pub struct Barebox {
    /// Directory of boot scripts, when one exists.
    env_boot: Option<PathBuf>,
    /// BLS entry directories Barebox would read.
    entry_dirs: Vec<(PathBuf, PathBuf)>,
    /// File holding `global.boot.default`.
    default_file: Option<PathBuf>,
}

impl Barebox {
    /// Where a Barebox environment is mounted or unpacked.
    const ENV_DIRS: [&'static str; 3] = ["/env", "/boot/env", "/mnt/env"];

    pub fn detect(roots: &BootRoots) -> Option<Barebox> {
        let mut env_boot = None;
        let mut default_file = None;

        // The environment lives at a fixed path on the running system, so a
        // scan aimed elsewhere must not read it.
        if !roots.host_state {
            return None;
        }

        for base in Self::ENV_DIRS {
            let dir = Path::new(base);
            if dir.join("boot").is_dir() {
                env_boot = Some(dir.join("boot"));
                let nv = dir.join("nv/boot.default");
                if nv.is_file() {
                    default_file = Some(nv);
                }
                break;
            }
        }

        // Barebox also reads BLS entries, but only claim those when a Barebox
        // environment is actually present - otherwise this would fight
        // systemd-boot over the same directory on every machine.
        env_boot.as_ref()?;

        let mut entry_dirs = Vec::new();
        for root in &roots.boot {
            let dir = root.join("loader/entries");
            if dir.is_dir() {
                entry_dirs.push((dir, root.clone()));
            }
        }

        Some(Barebox { env_boot, entry_dirs, default_file })
    }

    fn default_target(&self) -> Option<String> {
        let path = self.default_file.as_ref()?;
        Some(std::fs::read_to_string(path).ok()?.trim().to_string())
    }
}

impl Bootloader for Barebox {
    fn kind(&self) -> LoaderKind {
        LoaderKind::Barebox
    }

    fn capabilities(&self) -> Capabilities {
        // The default is a plain file, so it can be set. Boot scripts are
        // programs, so their command lines are not safely rewritable.
        Capabilities::SET_DEFAULT
    }

    fn confidence(&self) -> u8 {
        75
    }

    fn config_files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        if let Some(dir) = &self.env_boot {
            if let Ok(read) = std::fs::read_dir(dir) {
                files.extend(read.flatten().map(|e| e.path()).filter(|p| p.is_file()));
            }
        }
        if let Some(f) = &self.default_file {
            files.push(f.clone());
        }
        for (dir, _) in &self.entry_dirs {
            if let Ok(read) = std::fs::read_dir(dir) {
                files.extend(
                    read.flatten()
                        .map(|e| e.path())
                        .filter(|p| p.extension().is_some_and(|x| x == "conf")),
                );
            }
        }
        files
    }

    fn post_write_note(&self) -> Option<String> {
        Some(
            "Barebox boot entries are shell scripts; kernelctl reads their plain variable \
             assignments but cannot follow logic a script computes at boot"
                .to_string(),
        )
    }

    fn entries(&self, _ctx: &Context) -> Result<Vec<BootEntry>> {
        let mut out = Vec::new();
        let default = self.default_target().unwrap_or_default();

        if let Some(dir) = &self.env_boot {
            let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
                .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect())
                .unwrap_or_default();
            paths.sort();

            for path in paths {
                let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                let text = atomic::read_to_string(&path).unwrap_or_default();
                let script = parse_script(&name, &text);

                let mut entry =
                    BootEntry::new(LoaderKind::Barebox, &path, &name, &name);
                entry.kernel = script.kernel.as_deref().map(PathBuf::from);
                entry.initrds = script.initrd.iter().map(PathBuf::from).collect();
                entry.devicetree = script.devicetree.as_deref().map(PathBuf::from);
                entry.cmdline = script.bootargs.join(" ");

                if script.has_logic {
                    entry
                        .extra
                        .insert("note".into(), "script computes values at boot".into());
                }
                entry.extra.insert("type".into(), "boot script".into());
                // `global.boot.default` may list several targets in order.
                if default.split_whitespace().next() == Some(name.as_str()) {
                    entry.flags.insert(EntryFlags::DEFAULT);
                }
                out.push(entry);
            }
        }

        // Bootloader Spec entries on any partition Barebox can read.
        for (dir, root) in &self.entry_dirs {
            let mut entries = bls::load_dir(dir, root, LoaderKind::Barebox)?;
            for entry in entries.iter_mut() {
                entry.extra.insert("type".into(), "bootloader spec entry".into());
                if default.split_whitespace().next() == Some(entry.native_id.as_str()) {
                    entry.flags.insert(EntryFlags::DEFAULT);
                }
            }
            out.append(&mut entries);
        }

        Ok(out)
    }

    fn set_default(&self, ctx: &Context, entry: &BootEntry) -> Result<Vec<WriteOutcome>> {
        let path = self.default_file.clone().or_else(|| {
            // The nv directory exists even when the variable has never been
            // set, so a first-time write is normal.
            self.env_boot.as_ref()?.parent().map(|p| p.join("nv/boot.default"))
        });
        let path = path.ok_or_else(|| {
            crate::error::Error::validation(
                "no Barebox environment directory found to record the default in",
            )
        })?;

        ctx.preflight_write("set-default", &path)?;
        if ctx.dry_run {
            return Ok(Vec::new());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| crate::error::Error::io(parent, e))?;
        }
        Ok(vec![atomic::write_atomic(&path, format!("{}\n", entry.native_id).as_bytes())?])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRIPT: &str = "\
#!/bin/sh
global.bootm.image=/mnt/mmc1/zImage
global.bootm.oftree=/env/oftree
global.bootm.initrd=/mnt/mmc1/initramfs
global.linux.bootargs.dyn.root=\"root=PARTUUID=deadbeef:01 rw\"
global.linux.bootargs.dyn.console=\"console=ttyS0,115200\"
";

    #[test]
    fn reads_plain_variable_assignments() {
        let s = parse_script("mmc", SCRIPT);
        assert_eq!(s.kernel.as_deref(), Some("/mnt/mmc1/zImage"));
        assert_eq!(s.initrd.as_deref(), Some("/mnt/mmc1/initramfs"));
        assert_eq!(s.devicetree.as_deref(), Some("/env/oftree"));
        assert_eq!(
            s.bootargs.join(" "),
            "root=PARTUUID=deadbeef:01 rw console=ttyS0,115200"
        );
        assert!(!s.has_logic);
    }

    #[test]
    fn flags_scripts_that_compute_their_values() {
        // What we read from such a script may not be what it actually boots.
        let s = parse_script("dyn", "global.bootm.image=$(find_kernel)\n");
        assert!(s.has_logic);

        let s = parse_script("cond", "if [ -e /x ]; then\nglobal.bootm.image=/a\nfi\n");
        assert!(s.has_logic);
    }

    #[test]
    fn ignores_commands_and_comments() {
        let s = parse_script("x", "# a comment\nmkdir -p /tmp\necho a=b\nglobal.bootm.image=/z\n");
        assert_eq!(s.kernel.as_deref(), Some("/z"));
    }

    // ---- adapter behaviour ---------------------------------------------

    use crate::loaders::testsupport::{Fixture, TempTree};

    /// Build a Barebox environment inside a scratch tree.
    fn barebox(tag: &str) -> (TempTree, Barebox) {
        let tree = TempTree::new(tag);
        tree.file("env/boot/mmc", SCRIPT);
        tree.file("env/boot/net", "global.bootm.image=/net/zImage\n");
        tree.file("env/nv/boot.default", "mmc\n");
        let loader = Barebox {
            env_boot: Some(tree.path("env/boot")),
            entry_dirs: Vec::new(),
            default_file: Some(tree.path("env/nv/boot.default")),
        };
        (tree, loader)
    }

    #[test]
    fn lists_boot_scripts_with_the_default_marked() {
        let (_tree, loader) = barebox("barebox-entries");
        let fx = Fixture::rooted(BootRoots::default());

        let entries = loader.entries(&fx.context()).unwrap();
        assert_eq!(entries.len(), 2);

        let mmc = entries.iter().find(|e| e.native_id == "mmc").unwrap();
        assert!(mmc.is_default());
        assert_eq!(mmc.kernel.as_deref(), Some(Path::new("/mnt/mmc1/zImage")));
        assert!(mmc.cmdline.contains("PARTUUID=deadbeef"));
        assert!(!entries.iter().find(|e| e.native_id == "net").unwrap().is_default());
    }

    #[test]
    fn set_default_writes_the_nv_variable() {
        let (tree, loader) = barebox("barebox-setdefault");
        let fx = Fixture::rooted(BootRoots::default());
        let entries = loader.entries(&fx.context()).unwrap();
        let net = entries.iter().find(|e| e.native_id == "net").unwrap();

        loader.set_default(&fx.context(), net).unwrap();

        assert_eq!(tree.read("env/nv/boot.default").trim(), "net");
        let reread = loader.entries(&fx.context()).unwrap();
        assert!(reread.iter().find(|e| e.native_id == "net").unwrap().is_default());
    }

    #[test]
    fn also_reads_bootloader_spec_entries() {
        let tree = TempTree::new("barebox-bls");
        tree.file("env/boot/mmc", SCRIPT);
        tree.file(
            "loader/entries/linux.conf",
            "title Embedded Linux\nlinux /zImage\noptions root=/dev/mmcblk0p2 rw\n",
        );
        let loader = Barebox {
            env_boot: Some(tree.path("env/boot")),
            entry_dirs: vec![(tree.path("loader/entries"), tree.root.clone())],
            default_file: None,
        };

        let fx = Fixture::rooted(tree.roots());
        let entries = loader.entries(&fx.context()).unwrap();

        assert_eq!(entries.len(), 2);
        let bls = entries.iter().find(|e| e.native_id == "linux.conf").unwrap();
        assert_eq!(bls.title, "Embedded Linux");
        assert_eq!(bls.extra.get("type").map(String::as_str), Some("bootloader spec entry"));
    }

    #[test]
    fn does_not_claim_bls_entries_without_a_barebox_environment() {
        // Otherwise this would fight systemd-boot over loader/entries on
        // every ordinary machine.
        let tree = TempTree::new("barebox-absent");
        tree.file("loader/entries/x.conf", "title X\nlinux /vmlinuz\n");
        assert!(Barebox::detect(&tree.roots()).is_none());
    }
}
