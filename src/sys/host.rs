//! Facts about the machine kernelctl is running on.
//!
//! Gathered once at startup and passed around by reference: the values cannot
//! change while the process runs, and several of them (the module directory
//! listing in particular) are expensive enough that re-reading them per entry
//! would show up in the TUI's redraw path.

use std::path::Path;

use crate::model::{Arch, KernelVersion};

/// Immutable description of the running system.
#[derive(Debug, Clone)]
pub struct Host {
    /// `uname -n`.
    pub hostname: String,
    /// `uname -r`, e.g. `6.11.0-9-generic`.
    pub kernel_release: String,
    /// Parsed form of `kernel_release`, when it parses.
    pub kernel_version: Option<KernelVersion>,
    /// `uname -m`, normalized.
    pub arch: Arch,
    /// Raw `uname -m` string, kept for display.
    pub machine: String,
    /// True when the firmware booted this system via UEFI. Determines whether
    /// EFI-only bootloaders are even worth probing.
    pub is_efi: bool,
    /// Distribution name from os-release, if readable.
    pub distro: Option<String>,
}

impl Host {
    /// Probe the running system. Every field degrades to a safe default rather
    /// than failing, so this never returns an error - a machine with an
    /// unreadable /proc is still one we can list boot entries on.
    pub fn detect() -> Host {
        let uname = rustix::system::uname();
        let hostname = uname.nodename().to_string_lossy().into_owned();
        let kernel_release = uname.release().to_string_lossy().into_owned();
        let machine = uname.machine().to_string_lossy().into_owned();

        // /sys/firmware/efi only exists when the kernel booted via UEFI, which
        // makes its presence the canonical EFI test.
        let is_efi = Path::new("/sys/firmware/efi").is_dir();

        Host {
            kernel_version: KernelVersion::parse(&kernel_release),
            arch: Arch::from_machine(&machine),
            hostname,
            kernel_release,
            machine,
            is_efi,
            distro: read_os_release_name(),
        }
    }

    /// Does this kernel release string refer to the running kernel? Compares
    /// the parsed version where possible so that `6.11.0-9-generic` matches
    /// regardless of how the bootloader spelled it.
    pub fn is_running_release(&self, release: &str) -> bool {
        if release == self.kernel_release {
            return true;
        }
        match (&self.kernel_version, KernelVersion::parse(release)) {
            (Some(a), Some(b)) => a.raw == b.raw,
            _ => false,
        }
    }

    /// Short label for the header bar, e.g. `Arch Linux`.
    pub fn distro_label(&self) -> &str {
        self.distro.as_deref().unwrap_or("Linux")
    }

    /// Firmware type as shown in `status`.
    pub fn firmware(&self) -> &'static str {
        if self.is_efi {
            "UEFI"
        } else {
            "BIOS / legacy"
        }
    }
}

/// Pull `PRETTY_NAME` (falling back to `NAME`) out of os-release.
fn read_os_release_name() -> Option<String> {
    // /etc/os-release is the modern location and /usr/lib the vendor default;
    // on most systems the former is a symlink to the latter.
    let text = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .ok()?;
    parse_os_release_name(&text)
}

fn parse_os_release_name(text: &str) -> Option<String> {
    let mut fallback = None;
    for line in text.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = unquote_shell(value);
        match key.trim() {
            "PRETTY_NAME" => return Some(value),
            "NAME" => fallback = Some(value),
            _ => {}
        }
    }
    fallback
}

/// Strip the shell quoting os-release uses around values.
fn unquote_shell(value: &str) -> String {
    let v = value.trim();
    if v.len() >= 2 {
        let bytes = v.as_bytes();
        let first = bytes[0];
        if (first == b'"' || first == b'\'') && bytes[bytes.len() - 1] == first {
            return v[1..v.len() - 1].replace("\\\"", "\"").replace("\\\\", "\\");
        }
    }
    v.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_pretty_name_from_os_release() {
        let text = "NAME=\"Arch Linux\"\nPRETTY_NAME=\"Arch Linux\"\nID=arch\n";
        assert_eq!(parse_os_release_name(text).as_deref(), Some("Arch Linux"));
    }

    #[test]
    fn falls_back_to_name_when_pretty_name_absent() {
        let text = "NAME=Debian\nID=debian\n";
        assert_eq!(parse_os_release_name(text).as_deref(), Some("Debian"));
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        let text = "# a comment\n\nPRETTY_NAME='Ubuntu 24.04 LTS'\n";
        assert_eq!(parse_os_release_name(text).as_deref(), Some("Ubuntu 24.04 LTS"));
    }

    #[test]
    fn detects_host_without_panicking() {
        // The values differ per machine, but detection must always succeed and
        // must always report a non-empty kernel release.
        let host = Host::detect();
        assert!(!host.kernel_release.is_empty());
        assert!(!host.machine.is_empty());
    }
}
