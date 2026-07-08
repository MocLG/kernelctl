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
}

/// Boot directories checked even when they are not distinct mount points -
/// on a system without a separate /boot partition these are just directories
/// on the root filesystem, and they still hold the configs we want.
const STANDARD_BOOT_DIRS: &[&str] = &[
    "/boot",
    "/efi",
    "/boot/efi",
    "/mnt/boot",
];

/// Directories holding loader configuration outside the boot partition.
pub const CONFIG_DIRS: &[&str] = &[
    "/etc",
    "/etc/default",
    "/etc/kernel",
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
        let mounts = mounts::read_mounts().unwrap_or_default();

        let mut boot: Vec<PathBuf> = Vec::new();
        let push = |p: PathBuf, boot: &mut Vec<PathBuf>| {
            if p.is_dir() && !boot.contains(&p) {
                boot.push(p);
            }
        };

        for o in overrides {
            push(o.clone(), &mut boot);
        }
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

        let mut esp = mounts::esp_roots(&mounts);
        for o in overrides {
            if o.join("EFI").is_dir() && !esp.contains(o) {
                esp.insert(0, o.clone());
            }
        }

        BootRoots { boot, esp, mounts }
    }

    /// Every candidate directory, boot roots first then config directories.
    pub fn all_dirs(&self) -> Vec<PathBuf> {
        let mut out = self.boot.clone();
        for d in CONFIG_DIRS {
            let p = PathBuf::from(d);
            if p.is_dir() && !out.contains(&p) {
                out.push(p);
            }
        }
        out
    }

    /// Join `relative` onto each boot root and keep the paths that exist.
    ///
    /// This is the workhorse behind adapter detection: "is there a
    /// `loader/loader.conf` under any boot root?" is one call.
    pub fn find(&self, relative: &str) -> Vec<PathBuf> {
        self.boot
            .iter()
            .map(|root| root.join(relative))
            .filter(|p| p.exists())
            .collect()
    }

    /// First existing match for `relative` under any boot root.
    pub fn find_first(&self, relative: &str) -> Option<PathBuf> {
        self.boot.iter().map(|root| root.join(relative)).find(|p| p.exists())
    }

    /// First existing path from a list of absolute candidates.
    pub fn first_existing(candidates: &[&str]) -> Option<PathBuf> {
        candidates.iter().map(PathBuf::from).find(|p| p.exists())
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
    fn overrides_take_priority_over_discovery() {
        let tmp = TmpDir::new("override");
        let roots = BootRoots::discover(std::slice::from_ref(&tmp.0));
        assert_eq!(roots.boot.first(), Some(&tmp.0), "--boot-dir must win");
    }

    #[test]
    fn ignores_override_paths_that_do_not_exist() {
        let missing = PathBuf::from("/nonexistent/kernelctl-boot-root");
        let roots = BootRoots::discover(&[missing.clone()]);
        assert!(!roots.boot.contains(&missing));
    }

    #[test]
    fn find_returns_only_existing_paths() {
        let tmp = TmpDir::new("find");
        fs::create_dir_all(tmp.0.join("loader/entries")).unwrap();
        fs::write(tmp.0.join("loader/loader.conf"), b"timeout 4\n").unwrap();

        let roots = BootRoots::discover(std::slice::from_ref(&tmp.0));
        assert_eq!(roots.find_first("loader/loader.conf"), Some(tmp.0.join("loader/loader.conf")));
        assert!(roots.find_first("loader/nope.conf").is_none());
        assert!(roots.find("loader/loader.conf").len() >= 1);
    }

    #[test]
    fn discovery_never_duplicates_a_root() {
        let roots = BootRoots::discover(&[PathBuf::from("/boot"), PathBuf::from("/boot")]);
        let count = roots.boot.iter().filter(|p| *p == Path::new("/boot")).count();
        assert!(count <= 1, "roots must be deduplicated");
    }

    #[test]
    fn all_dirs_includes_config_directories() {
        let roots = BootRoots::discover(&[]);
        assert!(roots.all_dirs().contains(&PathBuf::from("/etc")));
    }
}
