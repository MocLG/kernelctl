//! The normalized data model every bootloader adapter maps onto.
//!
//! Bootloader configs disagree about almost everything: GRUB has nested
//! submenus and shell-ish syntax, systemd-boot has one flat key/value file per
//! entry, Limine uses slash-depth trees, EFI-stub entries live in NVRAM and
//! have no config file at all. Adapters absorb that variety and emit
//! [`BootEntry`] values so that the rest of the program - listing, diffing,
//! the TUI, cleanup - only ever deals with one shape.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::util::hash::short_hash;

/// CPU architecture a boot entry targets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    #[default]
    Unknown,
    X86_64,
    X86,
    Aarch64,
    Arm,
    Riscv64,
    Loongarch64,
    Ppc64le,
    S390x,
}

impl Arch {
    /// Map a `uname -m` style machine string onto a known architecture.
    pub fn from_machine(machine: &str) -> Arch {
        match machine.trim().to_ascii_lowercase().as_str() {
            "x86_64" | "amd64" | "x64" => Arch::X86_64,
            "i386" | "i486" | "i586" | "i686" | "x86" | "ia32" => Arch::X86,
            "aarch64" | "arm64" | "aa64" => Arch::Aarch64,
            m if m.starts_with("armv") || m == "arm" || m == "armhf" || m == "armel" => Arch::Arm,
            "riscv64" => Arch::Riscv64,
            "loongarch64" => Arch::Loongarch64,
            "ppc64le" | "powerpc64le" => Arch::Ppc64le,
            "s390x" => Arch::S390x,
            _ => Arch::Unknown,
        }
    }

    /// Infer the architecture from a kernel image filename. Each port names its
    /// bootable image differently, which makes the filename a reliable hint
    /// when the config itself does not say.
    pub fn from_kernel_image(path: &Path) -> Option<Arch> {
        let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
        // EFI executables encode the arch in the suffix (linuxx64.efi.stub,
        // grubaa64.efi, bootarm.efi).
        for (needle, arch) in [
            ("x64", Arch::X86_64),
            ("x86_64", Arch::X86_64),
            ("aa64", Arch::Aarch64),
            ("arm64", Arch::Aarch64),
            ("aarch64", Arch::Aarch64),
            ("riscv64", Arch::Riscv64),
            ("loongarch64", Arch::Loongarch64),
            ("ia32", Arch::X86),
        ] {
            if name.contains(needle) {
                return Some(arch);
            }
        }
        // Fall back to the traditional per-port image names.
        if name.starts_with("bzimage") || name.starts_with("vmlinuz") {
            // vmlinuz is used by x86 and by distro ARM64 kernels alike, so it
            // is only a weak signal; leave it to the caller's system default.
            None
        } else if name.starts_with("zimage") {
            Some(Arch::Arm)
        } else if name == "image" || name.starts_with("image.gz") {
            Some(Arch::Aarch64)
        } else {
            None
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::X86 => "x86",
            Arch::Aarch64 => "aarch64",
            Arch::Arm => "arm",
            Arch::Riscv64 => "riscv64",
            Arch::Loongarch64 => "loongarch64",
            Arch::Ppc64le => "ppc64le",
            Arch::S390x => "s390x",
            Arch::Unknown => "unknown",
        }
    }

    /// True when an entry built for `self` can run on `host`.
    pub fn runs_on(self, host: Arch) -> bool {
        match (self, host) {
            (a, h) if a == h => true,
            // 64-bit hosts can boot their 32-bit counterparts.
            (Arch::X86, Arch::X86_64) => true,
            (Arch::Arm, Arch::Aarch64) => true,
            (Arch::Unknown, _) | (_, Arch::Unknown) => true,
            _ => false,
        }
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which bootloader produced an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoaderKind {
    Grub2,
    GrubLegacy,
    SystemdBoot,
    Limine,
    Extlinux,
    Syslinux,
    Refind,
    Lilo,
    EfiStub,
    Barebox,
    Uki,
}

impl LoaderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LoaderKind::Grub2 => "grub2",
            LoaderKind::GrubLegacy => "grub-legacy",
            LoaderKind::SystemdBoot => "systemd-boot",
            LoaderKind::Limine => "limine",
            LoaderKind::Extlinux => "extlinux",
            LoaderKind::Syslinux => "syslinux",
            LoaderKind::Refind => "refind",
            LoaderKind::Lilo => "lilo",
            LoaderKind::EfiStub => "efi-stub",
            LoaderKind::Barebox => "barebox",
            LoaderKind::Uki => "uki",
        }
    }

    /// Human-facing name used in headers and status output.
    pub fn display_name(self) -> &'static str {
        match self {
            LoaderKind::Grub2 => "GRUB 2",
            LoaderKind::GrubLegacy => "GRUB Legacy",
            LoaderKind::SystemdBoot => "systemd-boot",
            LoaderKind::Limine => "Limine",
            LoaderKind::Extlinux => "extlinux / U-Boot",
            LoaderKind::Syslinux => "Syslinux",
            LoaderKind::Refind => "rEFInd",
            LoaderKind::Lilo => "LILO",
            LoaderKind::EfiStub => "EFI Stub (NVRAM)",
            LoaderKind::Barebox => "Barebox",
            LoaderKind::Uki => "Unified Kernel Image",
        }
    }
}

impl fmt::Display for LoaderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// State badges shown next to an entry.
///
/// Hand-rolled rather than pulled from `bitflags` - it is a handful of bits and
/// avoiding the dependency keeps the binary lean.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EntryFlags(pub u16);

impl EntryFlags {
    pub const NONE: EntryFlags = EntryFlags(0);
    /// Entry the bootloader boots when the menu times out.
    pub const DEFAULT: EntryFlags = EntryFlags(1 << 0);
    /// Entry queued for exactly one boot.
    pub const ONESHOT: EntryFlags = EntryFlags(1 << 1);
    /// Entry whose kernel matches the currently running one.
    pub const RUNNING: EntryFlags = EntryFlags(1 << 2);
    /// Kernel is booted directly by firmware with no bootloader in between.
    pub const EFI_STUB: EntryFlags = EntryFlags(1 << 3);
    /// Kernel or initrd referenced by the entry is missing from disk.
    pub const BROKEN: EntryFlags = EntryFlags(1 << 4);
    /// Entry targets an architecture this machine cannot execute.
    pub const FOREIGN_ARCH: EntryFlags = EntryFlags(1 << 5);
    /// Single-file bundle of kernel + initrd + cmdline.
    pub const UNIFIED: EntryFlags = EntryFlags(1 << 6);
    /// Recovery / single-user / fallback variant.
    pub const RECOVERY: EntryFlags = EntryFlags(1 << 7);
    /// Entry is only a container for nested entries (GRUB submenu, Limine dir).
    pub const SUBMENU: EntryFlags = EntryFlags(1 << 8);
    /// Entry chainloads another operating system rather than a Linux kernel.
    pub const CHAINLOAD: EntryFlags = EntryFlags(1 << 9);

    pub fn contains(self, other: EntryFlags) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn insert(&mut self, other: EntryFlags) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: EntryFlags) {
        self.0 &= !other.0;
    }

    pub fn set(&mut self, other: EntryFlags, on: bool) {
        if on {
            self.insert(other);
        } else {
            self.remove(other);
        }
    }

    /// Badges in display order, most significant to the user first.
    pub fn badges(self) -> Vec<&'static str> {
        let mut out = Vec::new();
        for (flag, label) in [
            (EntryFlags::DEFAULT, "DEFAULT"),
            (EntryFlags::ONESHOT, "ONESHOT"),
            (EntryFlags::RUNNING, "RUNNING"),
            (EntryFlags::EFI_STUB, "EFI-STUB"),
            (EntryFlags::UNIFIED, "UKI"),
            (EntryFlags::RECOVERY, "RECOVERY"),
            (EntryFlags::CHAINLOAD, "CHAINLOAD"),
            (EntryFlags::SUBMENU, "SUBMENU"),
            (EntryFlags::FOREIGN_ARCH, "FOREIGN"),
            (EntryFlags::BROKEN, "BROKEN"),
        ] {
            if self.contains(flag) {
                out.push(label);
            }
        }
        out
    }
}

impl std::ops::BitOr for EntryFlags {
    type Output = EntryFlags;
    fn bitor(self, rhs: EntryFlags) -> EntryFlags {
        EntryFlags(self.0 | rhs.0)
    }
}

/// A single bootable option, normalized across bootloaders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootEntry {
    /// Stable short identifier derived from the loader, source and native id.
    /// Survives reordering of the config file, which a positional index would
    /// not.
    pub id: String,

    /// Title as the boot menu would display it.
    pub title: String,

    /// Resolved path to the kernel image, if the entry has a separate one.
    /// `None` for UKIs and chainloaded entries.
    pub kernel: Option<PathBuf>,

    /// Initramfs images, in load order. Several bootloaders allow more than
    /// one (microcode first, then the real initrd).
    #[serde(default)]
    pub initrds: Vec<PathBuf>,

    /// Device tree blob, for the ARM/RISC-V boot paths that need one.
    #[serde(default)]
    pub devicetree: Option<PathBuf>,

    /// Kernel command line, already joined into one string.
    #[serde(default)]
    pub cmdline: String,

    /// Architecture this entry targets.
    pub arch: Arch,

    /// Parsed kernel version, when one could be extracted.
    #[serde(default)]
    pub version: Option<KernelVersion>,

    /// State badges.
    pub flags: EntryFlags,

    /// Which bootloader owns this entry.
    pub loader: LoaderKind,

    /// Config file (or pseudo-path such as `efivars:Boot0003`) the entry came
    /// from. Backup and edit operations write here.
    pub source: PathBuf,

    /// Identifier in the bootloader's own namespace: a GRUB menuentry id, a
    /// BLS filename, a Limine tree path, an EFI boot number. Mutations pass
    /// this back to the loader.
    pub native_id: String,

    /// Nesting depth for loaders with submenus. 0 is top level.
    #[serde(default)]
    pub depth: u8,

    /// Kernel image mtime, used as a build-date proxy in the table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_time: Option<SystemTime>,

    /// Size of the kernel image in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_size: Option<u64>,

    /// Loader-specific leftovers worth showing but not worth a struct field
    /// (BLS `machine-id`, Limine `protocol`, rEFInd `volume`, ...).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, String>,
}

impl BootEntry {
    /// Build an entry with the fields every adapter must supply, then let the
    /// caller fill in the optional ones.
    pub fn new(
        loader: LoaderKind,
        source: impl Into<PathBuf>,
        native_id: impl Into<String>,
        title: impl Into<String>,
    ) -> BootEntry {
        let source = source.into();
        let native_id = native_id.into();
        let id = Self::compute_id(loader, &source, &native_id);
        BootEntry {
            id,
            title: title.into(),
            kernel: None,
            initrds: Vec::new(),
            devicetree: None,
            cmdline: String::new(),
            arch: Arch::Unknown,
            version: None,
            flags: EntryFlags::NONE,
            loader,
            source,
            native_id,
            depth: 0,
            build_time: None,
            kernel_size: None,
            extra: BTreeMap::new(),
        }
    }

    /// Derive the stable id. Keyed on loader + source + native id so that two
    /// bootloaders exposing the same kernel still get distinct ids.
    fn compute_id(loader: LoaderKind, source: &Path, native_id: &str) -> String {
        let material = format!("{}\u{1}{}\u{1}{}", loader.as_str(), source.display(), native_id);
        format!("{}-{}", loader.as_str(), short_hash(material.as_bytes()))
    }

    pub fn is_default(&self) -> bool {
        self.flags.contains(EntryFlags::DEFAULT)
    }

    pub fn is_oneshot(&self) -> bool {
        self.flags.contains(EntryFlags::ONESHOT)
    }

    pub fn is_running(&self) -> bool {
        self.flags.contains(EntryFlags::RUNNING)
    }

    /// Every on-disk file the entry depends on, for existence checks and for
    /// deciding which files cleanup must not delete.
    pub fn referenced_files(&self) -> Vec<&Path> {
        let mut out = Vec::new();
        if let Some(k) = &self.kernel {
            out.push(k.as_path());
        }
        out.extend(self.initrds.iter().map(|p| p.as_path()));
        if let Some(dtb) = &self.devicetree {
            out.push(dtb.as_path());
        }
        out
    }

    /// Split the command line into individual parameters, respecting the
    /// quoting the kernel itself honours.
    pub fn cmdline_params(&self) -> Vec<String> {
        split_cmdline(&self.cmdline)
    }

    /// Does this entry match a user-supplied pattern? Accepts the full id, an
    /// unambiguous id prefix, a kernel version, or a case-insensitive substring
    /// of the title.
    pub fn matches(&self, pattern: &str) -> bool {
        if pattern.is_empty() {
            return false;
        }
        if self.id.eq_ignore_ascii_case(pattern) {
            return true;
        }
        if self.id.to_ascii_lowercase().starts_with(&pattern.to_ascii_lowercase()) {
            return true;
        }
        if self.native_id == pattern {
            return true;
        }
        if let Some(v) = &self.version {
            if v.raw == pattern {
                return true;
            }
        }
        let needle = pattern.to_ascii_lowercase();
        if self.title.to_ascii_lowercase().contains(&needle) {
            return true;
        }
        if let Some(k) = &self.kernel {
            if k.to_string_lossy().to_ascii_lowercase().contains(&needle) {
                return true;
            }
        }
        false
    }

    /// Rank a match so exact hits beat fuzzy ones when a pattern is ambiguous.
    /// Lower is better.
    pub fn match_rank(&self, pattern: &str) -> Option<u8> {
        if self.id.eq_ignore_ascii_case(pattern) || self.native_id == pattern {
            return Some(0);
        }
        if self.id.to_ascii_lowercase().starts_with(&pattern.to_ascii_lowercase()) {
            return Some(1);
        }
        if self.version.as_ref().is_some_and(|v| v.raw == pattern) {
            return Some(2);
        }
        if self.title.eq_ignore_ascii_case(pattern) {
            return Some(3);
        }
        if self.matches(pattern) {
            return Some(4);
        }
        None
    }
}

/// A parsed kernel version, ordered newest-first by the CLI and TUI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelVersion {
    /// The version exactly as it appeared, e.g. `6.11.0-9-generic`.
    pub raw: String,
    /// Numeric components: major, minor, patch.
    pub numbers: Vec<u64>,
    /// Distro suffix after the numeric part, e.g. `9-generic`.
    pub suffix: String,
}

impl KernelVersion {
    /// Parse a version string such as `6.11.0-9-generic` or `5.15.0-rc3`.
    pub fn parse(raw: &str) -> Option<KernelVersion> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        let mut numbers = Vec::new();
        let bytes = raw.as_bytes();
        let mut i = 0;

        // A version must begin with a digit; anything else is a name, not a
        // version.
        if !bytes.first().is_some_and(u8::is_ascii_digit) {
            return None;
        }
        while i < bytes.len() {
            if bytes[i].is_ascii_digit() {
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                numbers.push(raw[start..i].parse().ok()?);
                // Only a '.' continues the numeric part; '-' starts the suffix.
                if i < bytes.len() && bytes[i] == b'.' {
                    i += 1;
                    continue;
                }
            }
            break;
        }
        if numbers.is_empty() {
            return None;
        }
        Some(KernelVersion {
            raw: raw.to_string(),
            numbers,
            suffix: raw[i..].trim_start_matches(['-', '.', '_']).to_string(),
        })
    }

    /// Pull a kernel version out of a filename like `vmlinuz-6.11.0-9-generic`
    /// or `initramfs-6.11.0-9-generic.img`.
    pub fn from_filename(name: &str) -> Option<KernelVersion> {
        // Strip the well-known image prefixes, then the well-known suffixes.
        let stem = ["vmlinuz-", "vmlinux-", "kernel-", "bzImage-", "zImage-", "Image-",
                    "initramfs-", "initrd.img-", "initrd-", "System.map-", "config-"]
            .iter()
            .find_map(|p| name.strip_prefix(p))
            .unwrap_or(name);
        let stem = [".img", ".gz", ".efi", ".conf", ".old-dkms"]
            .iter()
            .fold(stem, |acc, s| acc.strip_suffix(s).unwrap_or(acc));
        // `initramfs-6.11.0-generic-fallback.img` -> drop the trailing tag.
        let stem = stem.strip_suffix("-fallback").unwrap_or(stem);
        // A conventional image name puts the version first once the prefix is
        // gone. Names that do not - UKIs such as `arch-linux-6.12.1.efi`, or
        // menu titles - fall back to searching for it.
        KernelVersion::parse(stem).or_else(|| KernelVersion::find_in(stem))
    }

    /// Find the first version-like substring anywhere in a name.
    ///
    /// UKI filenames and boot menu titles put the version in the middle
    /// (`arch-linux-6.12.1.efi`, `Ubuntu, with Linux 6.11.0-9-generic`), so a
    /// prefix-anchored parse misses it entirely.
    pub fn find_in(name: &str) -> Option<KernelVersion> {
        let bytes = name.as_bytes();
        for i in 0..bytes.len() {
            // Only consider the start of a digit run, so the scan does not
            // retry at every digit of the same number.
            if !bytes[i].is_ascii_digit() || (i > 0 && bytes[i - 1].is_ascii_digit()) {
                continue;
            }
            if let Some(v) = KernelVersion::parse(&name[i..]) {
                // Require at least `major.minor`: a lone number is far more
                // likely to be a date stamp, an EFI arch suffix such as the
                // 64 in bootx64, or part of a device name.
                if v.numbers.len() >= 2 {
                    return Some(v);
                }
            }
        }
        None
    }
}

impl fmt::Display for KernelVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl Ord for KernelVersion {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Compare numeric components element-wise; a missing component counts
        // as 0 so that 6.11 and 6.11.0 compare equal.
        let len = self.numbers.len().max(other.numbers.len());
        for i in 0..len {
            let a = self.numbers.get(i).copied().unwrap_or(0);
            let b = other.numbers.get(i).copied().unwrap_or(0);
            match a.cmp(&b) {
                std::cmp::Ordering::Equal => {}
                ord => return ord,
            }
        }
        // Same numbers: a release outranks a prerelease of the same version,
        // so an empty suffix sorts last (i.e. highest).
        match (self.suffix.is_empty(), other.suffix.is_empty()) {
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            _ => natural_cmp(&self.suffix, &other.suffix),
        }
    }
}

impl PartialOrd for KernelVersion {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Compare strings so embedded numbers order numerically: `-9-generic` sorts
/// before `-10-generic`, which a plain lexical compare gets backwards.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut ai, mut bi) = (a.as_bytes().iter().peekable(), b.as_bytes().iter().peekable());
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let (mut ap, mut bp) = (0usize, 0usize);
    loop {
        match (ai.peek().is_some(), bi.peek().is_some()) {
            (false, false) => return std::cmp::Ordering::Equal,
            (false, true) => return std::cmp::Ordering::Less,
            (true, false) => return std::cmp::Ordering::Greater,
            (true, true) => {}
        }
        let (ac, bc) = (ab[ap], bb[bp]);
        if ac.is_ascii_digit() && bc.is_ascii_digit() {
            let astart = ap;
            while ap < ab.len() && ab[ap].is_ascii_digit() {
                ap += 1;
                ai.next();
            }
            let bstart = bp;
            while bp < bb.len() && bb[bp].is_ascii_digit() {
                bp += 1;
                bi.next();
            }
            // Parse failures here mean a run of digits too long for u64;
            // fall back to comparing by length then lexically.
            let an: u64 = a[astart..ap].parse().unwrap_or(u64::MAX);
            let bn: u64 = b[bstart..bp].parse().unwrap_or(u64::MAX);
            match an.cmp(&bn) {
                std::cmp::Ordering::Equal => continue,
                ord => return ord,
            }
        }
        match ac.cmp(&bc) {
            std::cmp::Ordering::Equal => {
                ap += 1;
                bp += 1;
                ai.next();
                bi.next();
            }
            ord => return ord,
        }
    }
}

/// Split a kernel command line into parameters.
///
/// The kernel's own parser treats a quoted region as part of the surrounding
/// token, so `a="b c" d` is two parameters, not three. Quotes are preserved so
/// that a round trip through get/set does not change the meaning.
pub fn split_cmdline(cmdline: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let mut has_token = false;

    for ch in cmdline.chars() {
        match ch {
            '"' => {
                in_quote = !in_quote;
                current.push(ch);
                has_token = true;
            }
            c if c.is_whitespace() && !in_quote => {
                if has_token {
                    out.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            c => {
                current.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_distro_kernel_versions() {
        let v = KernelVersion::parse("6.11.0-9-generic").unwrap();
        assert_eq!(v.numbers, vec![6, 11, 0]);
        assert_eq!(v.suffix, "9-generic");
    }

    #[test]
    fn rejects_non_versions() {
        assert!(KernelVersion::parse("generic").is_none());
        assert!(KernelVersion::parse("").is_none());
    }

    #[test]
    fn orders_versions_newest_last() {
        let mut v: Vec<_> = ["6.9.3", "6.11.0-9-generic", "6.11.0-10-generic", "5.15.0"]
            .iter()
            .map(|s| KernelVersion::parse(s).unwrap())
            .collect();
        v.sort();
        let order: Vec<_> = v.iter().map(|k| k.raw.as_str()).collect();
        // -10 must sort after -9: a lexical compare would invert these.
        assert_eq!(order, vec!["5.15.0", "6.9.3", "6.11.0-9-generic", "6.11.0-10-generic"]);
    }

    #[test]
    fn release_outranks_prerelease() {
        let rc = KernelVersion::parse("6.12.0-rc1").unwrap();
        let rel = KernelVersion::parse("6.12.0").unwrap();
        assert!(rel > rc);
    }

    #[test]
    fn extracts_version_from_image_filenames() {
        assert_eq!(KernelVersion::from_filename("vmlinuz-6.11.0-9-generic").unwrap().raw, "6.11.0-9-generic");
        assert_eq!(KernelVersion::from_filename("initrd.img-6.8.0-40-generic").unwrap().raw, "6.8.0-40-generic");
        assert_eq!(KernelVersion::from_filename("initramfs-6.6.1-arch1.img").unwrap().raw, "6.6.1-arch1");
        assert!(KernelVersion::from_filename("vmlinuz").is_none());
    }

    #[test]
    fn finds_versions_embedded_mid_name() {
        // UKI filenames and menu titles put the version in the middle.
        assert_eq!(KernelVersion::from_filename("arch-linux-6.12.1").unwrap().raw, "6.12.1");
        assert_eq!(
            KernelVersion::find_in("Ubuntu, with Linux 6.11.0-9-generic").unwrap().raw,
            "6.11.0-9-generic"
        );
    }

    #[test]
    fn ignores_lone_numbers_that_are_not_versions() {
        // The 64 in an EFI arch suffix, and a bare date stamp, are not
        // versions; requiring major.minor rules both out.
        assert!(KernelVersion::find_in("bootx64.efi").is_none());
        assert!(KernelVersion::find_in("Arch Linux 20260801").is_none());
        assert!(KernelVersion::find_in("no digits here").is_none());
    }

    #[test]
    fn splits_cmdline_respecting_quotes() {
        let parts = split_cmdline(r#"root=UUID=abc ro quiet opt="a b" splash"#);
        assert_eq!(parts, vec!["root=UUID=abc", "ro", "quiet", r#"opt="a b""#, "splash"]);
    }

    #[test]
    fn ids_are_stable_and_distinct() {
        let a = BootEntry::new(LoaderKind::Grub2, "/boot/grub/grub.cfg", "entry-1", "A");
        let b = BootEntry::new(LoaderKind::Grub2, "/boot/grub/grub.cfg", "entry-1", "Different title");
        let c = BootEntry::new(LoaderKind::Grub2, "/boot/grub/grub.cfg", "entry-2", "A");
        // The title is display-only, so it must not affect the id.
        assert_eq!(a.id, b.id);
        assert_ne!(a.id, c.id);
        assert!(a.id.starts_with("grub2-"));
    }

    #[test]
    fn arch_compat_allows_32bit_on_64bit_host() {
        assert!(Arch::Arm.runs_on(Arch::Aarch64));
        assert!(Arch::X86.runs_on(Arch::X86_64));
        assert!(!Arch::X86_64.runs_on(Arch::Aarch64));
    }

    #[test]
    fn flags_render_badges_in_order() {
        let mut f = EntryFlags::NONE;
        f.insert(EntryFlags::RUNNING);
        f.insert(EntryFlags::DEFAULT);
        assert_eq!(f.badges(), vec!["DEFAULT", "RUNNING"]);
    }
}
