//! Fail-safe config writes.
//!
//! A truncated bootloader config is an unbootable machine, so kernelctl never
//! writes to a live config file in place. Every write:
//!
//! 1. copies the current file to `<name>.bak`,
//! 2. writes the new content to a temporary file in the *same* directory
//!    (a rename is only atomic within one filesystem),
//! 3. fsyncs that file so the data is on disk before it is visible,
//! 4. renames it over the target, which is atomic,
//! 5. fsyncs the containing directory so the rename itself survives a crash.
//!
//! If anything fails partway, the temporary file is removed and the original
//! is left untouched.

use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Permissions applied to a config file we are creating from scratch.
const DEFAULT_MODE: u32 = 0o644;

/// Removes the temporary file if the write does not reach its rename.
struct TempGuard {
    path: Option<PathBuf>,
}

impl TempGuard {
    fn new(path: PathBuf) -> TempGuard {
        TempGuard { path: Some(path) }
    }

    /// Called once the rename has succeeded, so the file must not be removed.
    fn defuse(&mut self) {
        self.path = None;
    }
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if let Some(p) = &self.path {
            // Best effort: we are already on an error path, and failing to
            // clean up a temp file must not mask the original error.
            let _ = fs::remove_file(p);
        }
    }
}

/// Result of an atomic write, so callers can tell the user what was preserved.
#[derive(Debug, Clone)]
pub struct WriteOutcome {
    pub target: PathBuf,
    /// Path of the `.bak` copy, absent when the target did not exist before.
    pub backup: Option<PathBuf>,
}

/// Write `contents` to `path` atomically, preserving a `.bak` copy first.
pub fn write_atomic(path: &Path, contents: &[u8]) -> Result<WriteOutcome> {
    write_atomic_opts(path, contents, true)
}

/// As [`write_atomic`], but the backup copy can be suppressed - used when the
/// caller has already snapshotted the file (a `restore`, for instance, would
/// otherwise overwrite the good backup with the bad config).
pub fn write_atomic_opts(path: &Path, contents: &[u8], backup: bool) -> Result<WriteOutcome> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));

    // Inherit the existing file's mode so a write never loosens permissions on
    // a config that was deliberately restricted.
    let existing_mode = fs::metadata(path).ok().map(|m| m.permissions().mode() & 0o7777);

    let backup_path = if backup && path.exists() {
        let bak = backup_path_for(path);
        fs::copy(path, &bak).map_err(|e| Error::io(&bak, e))?;
        Some(bak)
    } else {
        None
    };

    // The temp file is a dotfile in the target directory: same filesystem, so
    // the rename is atomic, and hidden so a half-written config never looks
    // like a real entry to a bootloader that globs the directory.
    let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp_path = dir.join(format!(".{file_name}.kernelctl.{}.tmp", std::process::id()));
    let mut guard = TempGuard::new(tmp_path.clone());

    {
        let mut file = File::create(&tmp_path).map_err(|e| Error::io(&tmp_path, e))?;
        file.set_permissions(fs::Permissions::from_mode(existing_mode.unwrap_or(DEFAULT_MODE)))
            .map_err(|e| Error::io(&tmp_path, e))?;
        file.write_all(contents).map_err(|e| Error::io(&tmp_path, e))?;
        // Force the data out before the rename makes it reachable, otherwise a
        // crash can leave the new name pointing at a zero-length file.
        file.sync_all().map_err(|e| Error::io(&tmp_path, e))?;
    }

    fs::rename(&tmp_path, path).map_err(|e| Error::io(path, e))?;
    guard.defuse();

    sync_dir(dir);

    Ok(WriteOutcome { target: path.to_path_buf(), backup: backup_path })
}

/// The `.bak` sibling for a config file.
pub fn backup_path_for(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    path.with_file_name(name)
}

/// fsync a directory so a rename within it is durable.
///
/// Failure is not fatal: some filesystems reject opening a directory for sync
/// and the rename has already succeeded either way, so we lose durability
/// rather than correctness.
fn sync_dir(dir: &Path) {
    if let Ok(handle) = File::open(dir) {
        let _ = handle.sync_all();
    }
}

/// Copy `<name>.bak` back over `<name>`, for undoing a change.
pub fn restore_backup(path: &Path) -> Result<()> {
    let bak = backup_path_for(path);
    if !bak.exists() {
        return Err(Error::validation(format!("no backup at {}", bak.display())));
    }
    let contents = fs::read(&bak).map_err(|e| Error::io(&bak, e))?;
    // Suppress the backup here: the current file is the bad one, and copying it
    // over the good `.bak` would destroy the only clean copy.
    write_atomic_opts(path, &contents, false)?;
    Ok(())
}

/// Read a file, attaching the path to any I/O error.
pub fn read_to_string(path: &Path) -> Result<String> {
    // from_utf8_lossy rather than a hard error: a stray non-UTF-8 byte in a
    // comment should not make a whole config unreadable.
    let bytes = fs::read(path).map_err(|e| Error::io(path, e))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scratch directory that cleans itself up.
    struct TmpDir(PathBuf);

    impl TmpDir {
        fn new(tag: &str) -> TmpDir {
            let base = std::env::temp_dir().join(format!(
                "kernelctl-test-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            fs::create_dir_all(&base).unwrap();
            TmpDir(base)
        }
        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn creates_file_without_backup_when_absent() {
        let dir = TmpDir::new("create");
        let target = dir.path("loader.conf");

        let out = write_atomic(&target, b"timeout 5\n").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "timeout 5\n");
        assert!(out.backup.is_none(), "nothing existed to back up");
    }

    #[test]
    fn backs_up_previous_content_before_overwriting() {
        let dir = TmpDir::new("backup");
        let target = dir.path("grub.cfg");
        fs::write(&target, b"original\n").unwrap();

        let out = write_atomic(&target, b"replaced\n").unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "replaced\n");
        let bak = out.backup.expect("a backup must be made");
        assert_eq!(fs::read_to_string(&bak).unwrap(), "original\n");
        assert_eq!(bak.file_name().unwrap(), "grub.cfg.bak");
    }

    #[test]
    fn preserves_existing_file_mode() {
        let dir = TmpDir::new("mode");
        let target = dir.path("secret.conf");
        fs::write(&target, b"a\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();

        write_atomic(&target, b"b\n").unwrap();

        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a rewrite must not loosen permissions");
    }

    #[test]
    fn leaves_no_temporary_files_behind() {
        let dir = TmpDir::new("clean");
        let target = dir.path("limine.conf");
        write_atomic(&target, b"timeout: 3\n").unwrap();

        let strays: Vec<_> = fs::read_dir(&dir.0)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(strays.is_empty(), "temp files left behind: {strays:?}");
    }

    #[test]
    fn restores_from_backup() {
        let dir = TmpDir::new("restore");
        let target = dir.path("extlinux.conf");
        fs::write(&target, b"good\n").unwrap();
        write_atomic(&target, b"bad\n").unwrap();

        restore_backup(&target).unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "good\n");
        // The backup must survive the restore so it can be applied twice.
        assert_eq!(fs::read_to_string(backup_path_for(&target)).unwrap(), "good\n");
    }

    #[test]
    fn restore_without_backup_is_an_error() {
        let dir = TmpDir::new("norestore");
        let target = dir.path("nothing.conf");
        fs::write(&target, b"x\n").unwrap();
        assert!(restore_backup(&target).is_err());
    }

    #[test]
    fn read_errors_carry_the_path() {
        let missing = Path::new("/nonexistent/kernelctl/definitely-not-here.conf");
        let err = read_to_string(missing).unwrap_err();
        assert!(err.is_not_found());
        assert!(err.to_string().contains("definitely-not-here.conf"));
    }
}
