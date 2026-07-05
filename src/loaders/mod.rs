//! Bootloader adapters and the discovery registry.
//!
//! Each adapter knows one bootloader's on-disk format and presents it through
//! the [`Bootloader`] trait. Adapters do not talk to each other and do not
//! know which one is "active" - discovery decides that by scoring every
//! adapter that finds its config present, so a machine with both GRUB and a
//! leftover syslinux directory still gets a sensible primary loader while
//! keeping the other visible under `--all`.

pub mod bls;
pub mod efivars;
pub mod scan;

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::model::{Arch, BootEntry, EntryFlags, KernelVersion, LoaderKind};
use crate::sys::atomic::WriteOutcome;
use crate::sys::{Host, Privileges};

pub use scan::BootRoots;

/// What an adapter is able to do. Commands check this before attempting a
/// change so the user gets "LILO does not support oneshot boot" instead of a
/// confusing failure partway through a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities(pub u16);

impl Capabilities {
    pub const NONE: Capabilities = Capabilities(0);
    /// Can persistently change which entry boots by default.
    pub const SET_DEFAULT: Capabilities = Capabilities(1 << 0);
    /// Can queue an entry for exactly one boot.
    pub const SET_ONESHOT: Capabilities = Capabilities(1 << 1);
    /// Can read and write the menu timeout.
    pub const TIMEOUT: Capabilities = Capabilities(1 << 2);
    /// Can rewrite an entry's kernel command line.
    pub const EDIT_CMDLINE: Capabilities = Capabilities(1 << 3);
    /// Can delete an entry outright.
    pub const REMOVE_ENTRY: Capabilities = Capabilities(1 << 4);
    /// Config is generated from a template, so direct edits are transient.
    pub const GENERATED: Capabilities = Capabilities(1 << 5);

    pub fn contains(self, other: Capabilities) -> bool {
        self.0 & other.0 == other.0
    }

    /// Capability names for the status output.
    pub fn names(self) -> Vec<&'static str> {
        [
            (Capabilities::SET_DEFAULT, "set-default"),
            (Capabilities::SET_ONESHOT, "set-next"),
            (Capabilities::TIMEOUT, "timeout"),
            (Capabilities::EDIT_CMDLINE, "cmdline-edit"),
            (Capabilities::REMOVE_ENTRY, "remove-entry"),
        ]
        .into_iter()
        .filter(|(c, _)| self.contains(*c))
        .map(|(_, n)| n)
        .collect()
    }
}

impl std::ops::BitOr for Capabilities {
    type Output = Capabilities;
    fn bitor(self, rhs: Capabilities) -> Capabilities {
        Capabilities(self.0 | rhs.0)
    }
}

/// How long a boot menu waits before booting the default.
///
/// Bootloaders spell the two special cases differently - GRUB uses `0` and
/// `-1`, systemd-boot uses `0` and `menu-force`, Limine uses `0` and `no` -
/// so the meaning is modelled here and each adapter renders it natively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timeout {
    /// Boot the default immediately, showing no menu.
    Immediate,
    /// Wait this many seconds.
    Seconds(u32),
    /// Show the menu and wait for a keypress indefinitely.
    Indefinite,
}

impl Timeout {
    /// Parse the user-facing forms accepted by `kernelctl timeout set`.
    pub fn parse(s: &str) -> Result<Timeout> {
        let s = s.trim();
        match s.to_ascii_lowercase().as_str() {
            "0" | "immediate" | "none" => Ok(Timeout::Immediate),
            "-1" | "never" | "indefinite" | "menu" | "forever" => Ok(Timeout::Indefinite),
            _ => s
                .parse::<u32>()
                .map(Timeout::Seconds)
                .map_err(|_| Error::validation(format!(
                    "invalid timeout '{s}': expected seconds, 0 for immediate, or 'never'"
                ))),
        }
    }
}

impl std::fmt::Display for Timeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Timeout::Immediate => write!(f, "0s (no menu)"),
            Timeout::Seconds(n) => write!(f, "{n}s"),
            Timeout::Indefinite => write!(f, "no timeout (wait for input)"),
        }
    }
}

/// Everything an adapter needs to do its job.
pub struct Context<'a> {
    pub host: &'a Host,
    pub privileges: &'a Privileges,
    pub roots: &'a BootRoots,
    /// Report what would change without writing anything.
    pub dry_run: bool,
}

impl Context<'_> {
    /// Pre-flight gate shared by every mutating operation: refuse without
    /// root, and refuse on a read-only filesystem, before opening any file.
    pub fn preflight_write(&self, action: &str, target: &Path) -> Result<()> {
        self.privileges.require(action)?;
        if self.roots.is_read_only(target) {
            return Err(Error::validation(format!(
                "{} is on a read-only filesystem; remount it rw first",
                target.display()
            )));
        }
        Ok(())
    }
}

/// One bootloader's implementation.
///
/// Only `kind`, `capabilities`, `config_files` and `entries` are required.
/// Every mutating operation defaults to reporting itself unsupported, so an
/// adapter for a read-only format is a handful of lines.
pub trait Bootloader {
    fn kind(&self) -> LoaderKind;

    /// What this adapter can change.
    fn capabilities(&self) -> Capabilities;

    /// Config files this loader owns, for `backup` and for showing the user
    /// what a change would touch.
    fn config_files(&self) -> Vec<PathBuf>;

    /// Parse the loader's configuration into normalized entries.
    fn entries(&self, ctx: &Context) -> Result<Vec<BootEntry>>;

    /// How sure detection is that this loader is the active one, 0-100.
    /// Discovery sorts by this, so the primary loader is the highest scorer.
    fn confidence(&self) -> u8 {
        50
    }

    /// Advice printed after a successful change, e.g. that a generated config
    /// will be overwritten on the next kernel upgrade.
    fn post_write_note(&self) -> Option<String> {
        None
    }

    fn set_default(&self, _ctx: &Context, _entry: &BootEntry) -> Result<Vec<WriteOutcome>> {
        Err(Error::unsupported(self.kind().display_name(), "changing the default entry"))
    }

    fn set_oneshot(&self, _ctx: &Context, _entry: &BootEntry) -> Result<Vec<WriteOutcome>> {
        Err(Error::unsupported(self.kind().display_name(), "one-shot boot entries"))
    }

    fn clear_oneshot(&self, _ctx: &Context) -> Result<Vec<WriteOutcome>> {
        Err(Error::unsupported(self.kind().display_name(), "one-shot boot entries"))
    }

    fn timeout(&self, _ctx: &Context) -> Result<Option<Timeout>> {
        Ok(None)
    }

    fn set_timeout(&self, _ctx: &Context, _timeout: Timeout) -> Result<Vec<WriteOutcome>> {
        Err(Error::unsupported(self.kind().display_name(), "menu timeout configuration"))
    }

    fn set_cmdline(
        &self,
        _ctx: &Context,
        _entry: &BootEntry,
        _cmdline: &str,
    ) -> Result<Vec<WriteOutcome>> {
        Err(Error::unsupported(self.kind().display_name(), "editing kernel parameters"))
    }

    fn remove_entry(&self, _ctx: &Context, _entry: &BootEntry) -> Result<Vec<WriteOutcome>> {
        Err(Error::unsupported(self.kind().display_name(), "removing entries"))
    }
}

/// Resolve a path as written in a bootloader config against a boot root.
///
/// Loader configs use paths relative to whichever partition the loader reads,
/// so `/vmlinuz-6.11` inside a systemd-boot entry means `<esp>/vmlinuz-6.11`,
/// not the file at the filesystem root. Where the same file exists in both
/// places the root-relative form is preferred, since that is what the loader
/// itself will resolve.
pub fn resolve_under(root: &Path, raw: &str) -> PathBuf {
    let cleaned = raw.trim().replace('\\', "/");
    let relative = cleaned.trim_start_matches('/');
    let under_root = root.join(relative);
    if under_root.exists() {
        return under_root;
    }
    // Fall back to interpreting it as an absolute path, which is how configs
    // on systems without a separate boot partition are usually written.
    let absolute = PathBuf::from(&cleaned);
    if absolute.is_absolute() && absolute.exists() {
        return absolute;
    }
    // Neither exists: return the root-relative form so the "missing file"
    // error names the path the bootloader would actually have looked for.
    under_root
}

/// Fill in the fields that depend on the running system rather than on the
/// config file, and flag entries that would not boot.
///
/// Adapters deliberately do not do this themselves: it is identical for every
/// loader, and keeping it here means a new adapter gets correct RUNNING and
/// BROKEN badges for free.
pub fn annotate(entries: &mut [BootEntry], host: &Host) {
    for entry in entries.iter_mut() {
        // Derive the version from the kernel filename when the config did not
        // supply one.
        if entry.version.is_none() {
            if let Some(kernel) = &entry.kernel {
                if let Some(name) = kernel.file_name().and_then(|n| n.to_str()) {
                    entry.version = KernelVersion::from_filename(name);
                }
            }
        }
        // A UKI names its version in the image filename, and most loaders put
        // it in the title, so that is the next best source.
        if entry.version.is_none() {
            entry.version = KernelVersion::from_filename(&entry.title);
        }

        if let Some(kernel) = &entry.kernel {
            if let Ok(meta) = std::fs::metadata(kernel) {
                entry.build_time = meta.modified().ok();
                entry.kernel_size = Some(meta.len());
            }
            if entry.arch == Arch::Unknown {
                entry.arch = Arch::from_kernel_image(kernel).unwrap_or(host.arch);
            }
        } else if entry.arch == Arch::Unknown {
            entry.arch = host.arch;
        }

        // RUNNING: the entry's kernel version matches `uname -r`. Compared on
        // the parsed version so it holds regardless of how the loader spelled
        // it, and only for entries that actually name a version.
        let running = entry.version.as_ref().is_some_and(|v| host.is_running_release(&v.raw));
        entry.flags.set(EntryFlags::RUNNING, running);

        // BROKEN: a referenced file is missing. Only checked for absolute
        // paths - a chainload entry pointing at another disk has nothing for
        // us to stat.
        let missing = entry.referenced_files().iter().any(|p| p.is_absolute() && !p.exists());
        entry.flags.set(EntryFlags::BROKEN, missing);

        entry.flags.set(EntryFlags::FOREIGN_ARCH, !entry.arch.runs_on(host.arch));

        // RECOVERY: recognised from the conventional title wording, which is
        // the only signal most loaders give.
        let title = entry.title.to_ascii_lowercase();
        let recovery = ["recovery", "rescue", "single user", "single-user", "fallback", "(safe"]
            .iter()
            .any(|needle| title.contains(needle));
        entry.flags.set(EntryFlags::RECOVERY, recovery);
    }
}

/// Sort entries the way a user expects to read them: default first, then the
/// running kernel, then newest version down, then title.
pub fn sort_entries(entries: &mut [BootEntry]) {
    entries.sort_by(|a, b| {
        b.is_default()
            .cmp(&a.is_default())
            .then_with(|| b.is_oneshot().cmp(&a.is_oneshot()))
            .then_with(|| b.is_running().cmp(&a.is_running()))
            // Tuple is (b, a) so versions compare descending - newest first.
            .then_with(|| match (&b.version, &a.version) {
                (Some(x), Some(y)) => x.cmp(y),
                // b has a version and a does not, so a sorts after b.
                (Some(_), None) => std::cmp::Ordering::Greater,
                (None, Some(_)) => std::cmp::Ordering::Less,
                (None, None) => std::cmp::Ordering::Equal,
            })
            .then_with(|| a.title.cmp(&b.title))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_timeout_forms() {
        assert_eq!(Timeout::parse("5").unwrap(), Timeout::Seconds(5));
        assert_eq!(Timeout::parse("0").unwrap(), Timeout::Immediate);
        assert_eq!(Timeout::parse("immediate").unwrap(), Timeout::Immediate);
        assert_eq!(Timeout::parse("-1").unwrap(), Timeout::Indefinite);
        assert_eq!(Timeout::parse("never").unwrap(), Timeout::Indefinite);
        assert_eq!(Timeout::parse(" 12 ").unwrap(), Timeout::Seconds(12));
        assert!(Timeout::parse("soon").is_err());
        assert!(Timeout::parse("2.5").is_err());
    }

    #[test]
    fn capabilities_report_their_names() {
        let caps = Capabilities::SET_DEFAULT | Capabilities::TIMEOUT;
        assert_eq!(caps.names(), vec!["set-default", "timeout"]);
        assert!(caps.contains(Capabilities::SET_DEFAULT));
        assert!(!caps.contains(Capabilities::SET_ONESHOT));
    }

    #[test]
    fn resolves_config_paths_against_the_boot_root() {
        let dir = std::env::temp_dir().join(format!("kernelctl-resolve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("vmlinuz-6.11.0"), b"x").unwrap();

        // A leading slash in a loader config is relative to the boot root.
        assert_eq!(resolve_under(&dir, "/vmlinuz-6.11.0"), dir.join("vmlinuz-6.11.0"));
        // Backslashes (EFI style) are normalized.
        assert_eq!(resolve_under(&dir, "\\vmlinuz-6.11.0"), dir.join("vmlinuz-6.11.0"));
        // A miss still names the root-relative path the loader would use.
        assert_eq!(resolve_under(&dir, "/absent"), dir.join("absent"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn entry(title: &str, version: Option<&str>) -> BootEntry {
        let mut e = BootEntry::new(LoaderKind::Grub2, "/boot/grub/grub.cfg", title, title);
        e.version = version.and_then(KernelVersion::parse);
        e
    }

    #[test]
    fn sorts_default_and_running_to_the_top() {
        let mut entries = vec![
            entry("old", Some("6.9.0")),
            entry("newest", Some("6.12.0")),
            entry("chosen", Some("6.10.0")),
        ];
        entries[2].flags.insert(EntryFlags::DEFAULT);

        sort_entries(&mut entries);

        let titles: Vec<_> = entries.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, vec!["chosen", "newest", "old"]);
    }

    #[test]
    fn sorts_versionless_entries_last() {
        let mut entries = vec![entry("no version", None), entry("has version", Some("6.1.0"))];
        sort_entries(&mut entries);
        assert_eq!(entries[0].title, "has version");
    }

    #[test]
    fn annotate_flags_missing_kernels_as_broken() {
        let host = Host::detect();
        let mut e = entry("Linux", Some("6.11.0"));
        e.kernel = Some(PathBuf::from("/boot/vmlinuz-does-not-exist-6.11.0"));
        let mut entries = vec![e];

        annotate(&mut entries, &host);

        assert!(entries[0].flags.contains(EntryFlags::BROKEN));
    }

    #[test]
    fn annotate_marks_the_running_kernel() {
        let host = Host::detect();
        let mut entries = vec![entry("Current", Some(&host.kernel_release))];
        annotate(&mut entries, &host);
        assert!(entries[0].flags.contains(EntryFlags::RUNNING));
    }

    #[test]
    fn annotate_detects_recovery_entries() {
        let host = Host::detect();
        let mut entries = vec![
            entry("Linux (recovery mode)", Some("6.11.0")),
            entry("Linux", Some("6.11.0")),
        ];
        annotate(&mut entries, &host);
        assert!(entries[0].flags.contains(EntryFlags::RECOVERY));
        assert!(!entries[1].flags.contains(EntryFlags::RECOVERY));
    }
}
