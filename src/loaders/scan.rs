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
//! Where to look for boot configuration.
//!
//! Every adapter needs the same question answered - "which directories on this
//! machine might hold boot files?" - and the answer depends on how the ESP is
//! mounted, which varies by distribution. Resolving it once here keeps the
//! adapters free of mount-point guesswork and gives `--boot-dir` a single
//! place to take effect.

use std::path::{Path, PathBuf};

use crate::sys::mounts::{self, MountPoint};

/// Boot-related roots discovered on this system.
#[derive(Debug, Clone, Default)]
pub struct BootRoots {
    /// Directories that may contain kernels and loader configs, best first.
    pub boot: Vec<PathBuf>,
    /// Subset of `boot` that looks like a mounted EFI System Partition.
    pub esp: Vec<PathBuf>,
    /// Mount table, cached so adapters need not re-read /proc/mounts.
    pub mounts: Vec<MountPoint>,
    /// Directories holding loader configuration outside the boot partition.
    /// Empty when the roots were constructed rather than discovered, which is
    /// what keeps a scoped scan from reading the host's /etc.
    pub config: Vec<PathBuf>,
    /// May adapters read host-global state that has no configurable location -
    /// EFI NVRAM, Barebox's /env? False for a constructed scan, so pointing at
    /// a rescue image does not pick up the running machine's firmware entries.
    pub host_state: bool,
}

/// Directories holding loader configuration outside the boot partition.
const CONFIG_DIRS: &[&str] = &["/etc", "/etc/default", "/etc/kernel"];

/// Boot directories checked even when they are not distinct mount points -
/// on a system without a separate /boot partition these are just directories
/// on the root filesystem, and they still hold the configs we want.
const STANDARD_BOOT_DIRS: &[&str] = &[
    "/boot",
    "/efi",
    "/boot/efi",
    "/mnt/boot",
];

impl BootRoots {
    /// Probe the system for boot roots.
    ///
    /// `overrides` come from `--boot-dir` and take priority over everything
    /// discovered automatically, which is what makes it possible to inspect an
    /// ESP mounted somewhere unusual, or a disk image mounted for repair.
    pub fn discover(overrides: &[PathBuf]) -> BootRoots {
        // A failure to read /proc/mounts is survivable: we fall back to the
        // standard directory list, which is right on most systems anyway.
        // The mount table is still wanted when scoped, for free-space
        // reporting and read-only checks on the paths being examined.
        let mounts = mounts::read_mounts().unwrap_or_default();

        // An explicit --boot-dir replaces auto-discovery rather than adding
        // to it. The point of the flag is to examine a specific tree - a
        // mounted ESP, a rescue image - and mixing the running system's
        // /boot, /etc and firmware into that answer describes neither
        // accurately.
        if !overrides.is_empty() {
            let mut boot: Vec<PathBuf> = Vec::new();
            for o in overrides.iter().filter(|p| p.is_dir()) {
                // --boot-dir is repeatable, so the same path may be given
                // twice; scanning it twice would duplicate every entry.
                if !boot.contains(o) {
                    boot.push(o.clone());
                }
            }
            let esp = boot.iter().filter(|p| p.join("EFI").is_dir()).cloned().collect();
            return BootRoots { boot, esp, mounts, config: Vec::new(), host_state: false };
        }

        let mut boot: Vec<PathBuf> = Vec::new();
        let push = |p: PathBuf, boot: &mut Vec<PathBuf>| {
            if p.is_dir() && !boot.contains(&p) {
                boot.push(p);
            }
        };

        for m in mounts::boot_mounts(&mounts) {
            // The root filesystem is not itself a boot root; its /boot is,
            // and that is covered by the standard list below.
            if m.target != Path::new("/") {
                push(m.target.clone(), &mut boot);
            }
        }
        for d in STANDARD_BOOT_DIRS {
            push(PathBuf::from(d), &mut boot);
        }
        // Removable media that happens to carry a boot directory.
        for entry in glob_dirs("/run/media/*/boot").into_iter().chain(glob_dirs("/media/*/boot")) {
            push(entry, &mut boot);
        }

        let esp = mounts::esp_roots(&mounts);

        let config = CONFIG_DIRS
            .iter()
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .collect();

        BootRoots { boot, esp, mounts, config, host_state: true }
    }

    /// Find a config file under the configuration directories.
    ///
    /// Returns `None` for a constructed scan, which is what stops an adapter
    /// reaching into the host's /etc when it was asked to look elsewhere.
    pub fn find_config(&self, relative: &str) -> Option<PathBuf> {
        self.config.iter().map(|dir| dir.join(relative)).find(|p| p.is_file())
    }

    /// Is `path` on a filesystem mounted read-only? Reported as a pre-flight
    /// warning, since the write would otherwise fail with a bare EROFS.
    pub fn is_read_only(&self, path: &Path) -> bool {
        mounts::mount_for(&self.mounts, path).is_some_and(|m| m.is_read_only())
    }
}

/// Expand a glob and keep the directories it matched.
fn glob_dirs(pattern: &str) -> Vec<PathBuf> {
    match glob::glob(pattern) {
        Ok(paths) => paths.flatten().filter(|p| p.is_dir()).collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct TmpDir(PathBuf);

    impl TmpDir {
        fn new(tag: &str) -> TmpDir {
            let base = std::env::temp_dir()
                .join(format!("kernelctl-scan-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&base);
            fs::create_dir_all(&base).unwrap();
            TmpDir(base)
        }
    }

    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn an_override_replaces_discovery_rather_than_extending_it() {
        let tmp = TmpDir::new("override");
        let roots = BootRoots::discover(std::slice::from_ref(&tmp.0));

        assert_eq!(roots.boot, vec![tmp.0.clone()], "--boot-dir must be the whole search");
        // The running system's /boot and /etc must not leak into a scan that
        // was aimed somewhere specific.
        assert!(!roots.boot.contains(&PathBuf::from("/boot")));
        assert!(roots.config.is_empty());
        assert!(!roots.host_state);
    }

    #[test]
    fn ignores_override_paths_that_do_not_exist() {
        let missing = PathBuf::from("/nonexistent/kernelctl-boot-root");
        let roots = BootRoots::discover(&[missing.clone()]);
        assert!(!roots.boot.contains(&missing));
    }

    #[test]
    fn discovered_roots_may_read_host_state_but_constructed_ones_may_not() {
        assert!(BootRoots::discover(&[]).host_state);
        // The default is what tests and scoped scans use; it must not reach
        // into /etc or firmware.
        let scoped = BootRoots::default();
        assert!(!scoped.host_state);
        assert!(scoped.config.is_empty());
        assert_eq!(scoped.find_config("default/grub"), None);
    }

    #[test]
    fn discovery_finds_the_config_directories() {
        let roots = BootRoots::discover(&[]);
        assert!(roots.config.contains(&PathBuf::from("/etc")));
    }

    #[test]
    fn discovery_never_duplicates_a_root() {
        let roots = BootRoots::discover(&[PathBuf::from("/boot"), PathBuf::from("/boot")]);
        let count = roots.boot.iter().filter(|p| *p == Path::new("/boot")).count();
        assert!(count <= 1, "roots must be deduplicated");
    }
}
