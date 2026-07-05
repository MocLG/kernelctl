//! Scratch boot trees for adapter tests.
//!
//! The adapters are mostly filesystem shape detection, so testing them means
//! building a directory that looks like a real ESP or /boot and pointing
//! discovery at it. This builds those trees and removes them afterwards.

#![cfg(test)]

use std::path::{Path, PathBuf};

use crate::loaders::{BootRoots, Context};
use crate::sys::{Host, Privileges};

/// A temporary directory tree that deletes itself on drop.
pub struct TempTree {
    pub root: PathBuf,
}

impl TempTree {
    pub fn new(tag: &str) -> TempTree {
        // Include the thread id so tests running in parallel never collide.
        let root = std::env::temp_dir().join(format!(
            "kernelctl-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        TempTree { root }
    }

    /// Write a file, creating parent directories as needed.
    pub fn file(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, contents).unwrap();
        path
    }

    /// Create an empty directory.
    pub fn dir(&self, relative: &str) -> PathBuf {
        let path = self.root.join(relative);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    pub fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.root.join(relative)).unwrap()
    }

    /// Boot roots that see only this tree, so tests never touch the real /boot.
    pub fn roots(&self) -> BootRoots {
        let mut roots = BootRoots::default();
        roots.boot = vec![self.root.clone()];
        roots
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Host and privilege values held alive for the lifetime of a [`Context`].
pub struct Fixture {
    pub host: Host,
    pub privileges: Privileges,
    pub roots: BootRoots,
}

impl Fixture {
    /// A fixture that claims root, so write paths can be exercised against a
    /// scratch tree without the test needing real privileges.
    pub fn rooted(roots: BootRoots) -> Fixture {
        Fixture {
            host: Host::detect(),
            privileges: Privileges { root: true, uid: 0, via_sudo: false },
            roots,
        }
    }

    /// A fixture without privileges, for checking that writes are refused.
    pub fn unprivileged(roots: BootRoots) -> Fixture {
        Fixture {
            host: Host::detect(),
            privileges: Privileges { root: false, uid: 1000, via_sudo: false },
            roots,
        }
    }

    pub fn context(&self) -> Context<'_> {
        Context {
            host: &self.host,
            privileges: &self.privileges,
            roots: &self.roots,
            dry_run: false,
        }
    }
}

/// A minimal kernel image, so existence checks and size reporting see a real
/// file rather than an empty one.
pub fn fake_kernel(tree: &TempTree, relative: &str) -> PathBuf {
    tree.file(relative, "\u{7f}ELF fake kernel image for tests\n")
}

/// Assert that a path exists, with a message naming it.
pub fn assert_exists(path: &Path) {
    assert!(path.exists(), "expected {} to exist", path.display());
}
