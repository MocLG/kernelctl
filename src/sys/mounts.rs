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
//! Mounted filesystems, ESP discovery and free-space reporting.
//!
//! `status` reports how much room is left on the boot partition, and the
//! cleanup command needs to know which filesystem a file lives on before it
//! claims removing it will free space. Both come from here.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// One line of `/proc/mounts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountPoint {
    pub source: String,
    pub target: PathBuf,
    pub fstype: String,
    pub options: String,
}

impl MountPoint {
    /// True when the filesystem is mounted read-only, which is the usual
    /// reason a write to a perfectly valid path fails.
    pub fn is_read_only(&self) -> bool {
        self.options.split(',').any(|o| o == "ro")
    }

    /// Filesystems that can hold an EFI System Partition.
    pub fn is_efi_capable(&self) -> bool {
        matches!(self.fstype.as_str(), "vfat" | "msdos" | "fat" | "fat32")
    }
}

/// Parse `/proc/mounts`.
pub fn read_mounts() -> Result<Vec<MountPoint>> {
    let text = std::fs::read_to_string("/proc/mounts")
        .map_err(|e| Error::io("/proc/mounts", e))?;
    Ok(parse_mounts(&text))
}

fn parse_mounts(text: &str) -> Vec<MountPoint> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let source = f.next()?;
            let target = f.next()?;
            let fstype = f.next()?;
            let options = f.next().unwrap_or_default();
            Some(MountPoint {
                source: unescape_octal(source),
                target: PathBuf::from(unescape_octal(target)),
                fstype: fstype.to_string(),
                options: options.to_string(),
            })
        })
        .collect()
}

/// /proc/mounts escapes space, tab, newline and backslash as three-digit octal
/// (`\040`), so a mount point with a space in it has to be decoded.
fn unescape_octal(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let digits = &s[i + 1..i + 4];
            if let Ok(code) = u8::from_str_radix(digits, 8) {
                out.push(code as char);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Free-space snapshot for one filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpaceInfo {
    pub total: u64,
    /// Free space usable by an unprivileged user (excludes reserved blocks).
    pub available: u64,
    /// Free space including the root-only reserve.
    pub free: u64,
}

impl SpaceInfo {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }

    /// Used space as a percentage of the total, 0 when the total is unknown.
    pub fn used_percent(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        self.used() as f64 / self.total as f64 * 100.0
    }

    /// Boot partitions are small and easy to fill; warn before a kernel
    /// install fails for want of a few megabytes.
    pub fn is_low(&self) -> bool {
        self.used_percent() >= 90.0 || self.available < 32 * 1024 * 1024
    }
}

/// statvfs the filesystem containing `path`.
pub fn space_for(path: &Path) -> Result<SpaceInfo> {
    let stat = rustix::fs::statvfs(path).map_err(|e| Error::io(path, e.into()))?;
    // f_frsize is the fragment size that the block counts are expressed in;
    // f_bsize is only the preferred I/O size and is wrong here on some
    // filesystems. Fall back to f_bsize when f_frsize is unset.
    let block = if stat.f_frsize > 0 { stat.f_frsize } else { stat.f_bsize };
    Ok(SpaceInfo {
        total: stat.f_blocks.saturating_mul(block),
        available: stat.f_bavail.saturating_mul(block),
        free: stat.f_bfree.saturating_mul(block),
    })
}

/// The mount point that `path` lives on: the longest mounted prefix of it.
pub fn mount_for<'a>(mounts: &'a [MountPoint], path: &Path) -> Option<&'a MountPoint> {
    mounts
        .iter()
        .filter(|m| path.starts_with(&m.target))
        .max_by_key(|m| m.target.as_os_str().len())
}

/// Mount points that plausibly hold boot files, best candidate first.
///
/// A vfat filesystem under a boot path is almost certainly the ESP; a separate
/// /boot partition is the next most interesting; the root filesystem is the
/// fallback for systems with no separate boot partition at all.
pub fn boot_mounts(mounts: &[MountPoint]) -> Vec<&MountPoint> {
    const BOOT_PATHS: [&str; 5] = ["/efi", "/boot/efi", "/boot", "/mnt/boot", "/"];

    let mut found: Vec<&MountPoint> = Vec::new();
    for candidate in BOOT_PATHS {
        if let Some(m) = mounts.iter().find(|m| m.target == Path::new(candidate)) {
            found.push(m);
        }
    }
    // A vfat mount anywhere else is still worth reporting - some systems mount
    // the ESP at a non-standard path.
    for m in mounts.iter().filter(|m| m.is_efi_capable()) {
        if !found.iter().any(|f| f.target == m.target) {
            found.push(m);
        }
    }
    found
}

/// Directories that could contain an EFI System Partition's contents.
///
/// Returned in preference order and filtered to those that exist, so callers
/// can take the first hit.
pub fn esp_roots(mounts: &[MountPoint]) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = mounts
        .iter()
        .filter(|m| m.is_efi_capable() && m.target.join("EFI").is_dir())
        .map(|m| m.target.clone())
        .collect();

    // Cover the standard locations even when /proc/mounts is unavailable, as
    // it is inside some containers.
    for fallback in ["/efi", "/boot/efi", "/boot"] {
        let p = PathBuf::from(fallback);
        if p.join("EFI").is_dir() && !roots.contains(&p) {
            roots.push(p);
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
/dev/nvme0n1p2 / ext4 rw,relatime 0 0
/dev/nvme0n1p1 /boot/efi vfat rw,relatime,fmask=0022 0 0
/dev/nvme0n1p3 /boot ext4 rw,relatime 0 0
proc /proc proc rw,nosuid,nodev,noexec 0 0
/dev/sdb1 /mnt/my\\040disk ext4 ro,relatime 0 0
";

    #[test]
    fn parses_proc_mounts() {
        let mounts = parse_mounts(SAMPLE);
        assert_eq!(mounts.len(), 5);
        assert_eq!(mounts[1].target, Path::new("/boot/efi"));
        assert_eq!(mounts[1].fstype, "vfat");
        assert!(mounts[1].is_efi_capable());
        assert!(!mounts[0].is_efi_capable());
    }

    #[test]
    fn decodes_octal_escapes_in_mount_points() {
        let mounts = parse_mounts(SAMPLE);
        assert_eq!(mounts[4].target, Path::new("/mnt/my disk"));
    }

    #[test]
    fn detects_read_only_mounts() {
        let mounts = parse_mounts(SAMPLE);
        assert!(mounts[4].is_read_only());
        assert!(!mounts[0].is_read_only());
    }

    #[test]
    fn picks_longest_matching_mount() {
        let mounts = parse_mounts(SAMPLE);
        // /boot/efi/EFI/... must resolve to /boot/efi, not / or /boot.
        let m = mount_for(&mounts, Path::new("/boot/efi/EFI/systemd/systemd-bootx64.efi")).unwrap();
        assert_eq!(m.target, Path::new("/boot/efi"));

        let m = mount_for(&mounts, Path::new("/boot/vmlinuz-6.11.0")).unwrap();
        assert_eq!(m.target, Path::new("/boot"));

        let m = mount_for(&mounts, Path::new("/home/user/file")).unwrap();
        assert_eq!(m.target, Path::new("/"));
    }

    #[test]
    fn orders_boot_mounts_by_relevance() {
        let mounts = parse_mounts(SAMPLE);
        let boot = boot_mounts(&mounts);
        let targets: Vec<_> = boot.iter().map(|m| m.target.to_string_lossy().into_owned()).collect();
        assert_eq!(targets, vec!["/boot/efi", "/boot", "/"]);
    }

    #[test]
    fn space_percentages_are_sane() {
        let s = SpaceInfo { total: 1000, available: 200, free: 250 };
        assert_eq!(s.used(), 750);
        assert!((s.used_percent() - 75.0).abs() < f64::EPSILON);
        assert!(s.is_low(), "under 32 MiB available counts as low");

        let roomy = SpaceInfo {
            total: 100 * 1024 * 1024 * 1024,
            available: 50 * 1024 * 1024 * 1024,
            free: 50 * 1024 * 1024 * 1024,
        };
        assert!(!roomy.is_low());
    }

    #[test]
    fn space_info_handles_zero_total() {
        let s = SpaceInfo { total: 0, available: 0, free: 0 };
        assert_eq!(s.used_percent(), 0.0);
    }

    #[test]
    fn statvfs_reads_the_running_filesystem() {
        let s = space_for(Path::new("/")).unwrap();
        assert!(s.total > 0, "root filesystem should report a size");
        assert!(s.free <= s.total);
    }
}
