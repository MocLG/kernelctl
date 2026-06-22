//! Privilege detection and the read-only fallback.
//!
//! Reading boot configuration works fine as an unprivileged user on most
//! systems, so `status` and `list` must never demand root. Writes are gated:
//! rather than letting a config write fail halfway through, mutating commands
//! call [`Privileges::require`] up front and exit with an actionable message.

use std::path::Path;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Privileges {
    /// Effective uid is 0.
    pub root: bool,
    /// Real uid, used to tell "logged in as root" from "invoked via sudo".
    pub uid: u32,
    /// True when we were started through sudo.
    pub via_sudo: bool,
}

impl Privileges {
    pub fn detect() -> Privileges {
        let euid = rustix::process::geteuid();
        let uid = rustix::process::getuid();
        Privileges {
            root: euid.is_root(),
            uid: uid.as_raw(),
            // sudo exports the original uid; its presence distinguishes an
            // escalated session from a genuine root login.
            via_sudo: std::env::var_os("SUDO_UID").is_some(),
        }
    }

    /// Badge for the TUI header bar.
    pub fn badge(&self) -> &'static str {
        if self.root {
            "[ROOT]"
        } else {
            "[USER]"
        }
    }

    /// Reject the operation unless we are root. `action` is the command name,
    /// so the hint can suggest the exact line to re-run.
    pub fn require(&self, action: &str) -> Result<()> {
        if self.root {
            Ok(())
        } else {
            Err(Error::needs_root(action))
        }
    }

    /// Can we realistically modify this path? Used to warn before starting a
    /// multi-file operation rather than failing on the third file.
    pub fn can_write(&self, path: &Path) -> bool {
        if self.root {
            return true;
        }
        // An existing file is writable if access(2) says so; for a new file the
        // question is whether the parent directory is writable.
        let target = if path.exists() {
            path
        } else {
            match path.parent() {
                Some(p) => p,
                None => return false,
            }
        };
        rustix::fs::access(target, rustix::fs::Access::WRITE_OK).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_reflects_root_state() {
        assert_eq!(Privileges { root: true, uid: 0, via_sudo: false }.badge(), "[ROOT]");
        assert_eq!(Privileges { root: false, uid: 1000, via_sudo: false }.badge(), "[USER]");
    }

    #[test]
    fn require_rejects_unprivileged_writes() {
        let user = Privileges { root: false, uid: 1000, via_sudo: false };
        let err = user.require("set-default").unwrap_err();
        assert!(matches!(err, Error::NeedsRoot { .. }));
        assert!(err.hint().unwrap().contains("sudo kernelctl set-default"));

        let root = Privileges { root: true, uid: 0, via_sudo: false };
        assert!(root.require("set-default").is_ok());
    }

    #[test]
    fn root_can_write_anywhere() {
        let root = Privileges { root: true, uid: 0, via_sudo: false };
        assert!(root.can_write(Path::new("/boot/does-not-exist")));
    }

    #[test]
    fn detect_reports_consistent_state() {
        let p = Privileges::detect();
        assert_eq!(p.root, p.uid == 0 || rustix::process::geteuid().is_root());
    }
}
