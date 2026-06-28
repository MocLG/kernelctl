//! Running the external helpers some bootloaders require.
//!
//! A few operations have no safe file-level equivalent and must go through the
//! vendor tool: EFI NVRAM is written with `efibootmgr`, GRUB's persistent
//! environment block is a fixed-size record only `grub-editenv` maintains
//! correctly, and systemd-boot's `bootctl` knows about ESP layout we should
//! not second-guess. Everything else is done by editing files directly, so
//! kernelctl still works on a system with none of these installed.

use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Command;

use crate::error::{Error, Result};

/// Look a binary up in PATH, plus the sbin directories that are commonly
/// missing from a non-root PATH but hold exactly the tools we want.
pub fn which(tool: &str) -> Option<PathBuf> {
    if tool.contains('/') {
        let p = PathBuf::from(tool);
        return p.is_file().then_some(p);
    }

    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let extra = ["/usr/sbin", "/sbin", "/usr/local/sbin", "/usr/bin", "/bin"];

    std::env::split_paths(&path_var)
        .chain(extra.iter().map(PathBuf::from))
        .map(|dir| dir.join(tool))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &std::path::Path) -> bool {
    path.is_file() && rustix::fs::access(path, rustix::fs::Access::EXEC_OK).is_ok()
}

/// Resolve a helper or explain how to get it.
pub fn require(tool: &str, hint: &str) -> Result<PathBuf> {
    which(tool).ok_or_else(|| Error::MissingTool {
        tool: tool.to_string(),
        hint: hint.to_string(),
    })
}

/// Captured result of a successful helper invocation.
#[derive(Debug, Clone)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
}

/// Run a helper and capture its output, turning a non-zero exit into an error.
pub fn run<I, S>(tool: &str, args: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let path = which(tool).ok_or_else(|| Error::MissingTool {
        tool: tool.to_string(),
        hint: format!("install the package providing '{tool}'"),
    })?;

    let out = Command::new(&path)
        .args(args)
        // Some of these tools localize their output, which we then parse.
        // Pinning the locale keeps parsing stable across systems.
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .output()
        .map_err(|e| Error::io(&path, e))?;

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    if out.status.success() {
        Ok(Output { stdout, stderr })
    } else {
        Err(Error::ToolFailed {
            tool: tool.to_string(),
            status: match out.status.code() {
                Some(c) => format!("exit {c}"),
                None => "killed by signal".to_string(),
            },
            stderr,
        })
    }
}

/// Run a helper only to learn whether it succeeds.
pub fn probe<I, S>(tool: &str, args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run(tool, args).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_binary_that_exists() {
        // `sh` is required by POSIX to be in the standard PATH.
        assert!(which("sh").is_some());
    }

    #[test]
    fn returns_none_for_missing_binary() {
        assert!(which("kernelctl-definitely-not-a-real-binary").is_none());
    }

    #[test]
    fn absolute_paths_are_checked_directly() {
        assert!(which("/bin/sh").is_some() || which("/usr/bin/sh").is_some());
        assert!(which("/nonexistent/binary").is_none());
    }

    #[test]
    fn runs_a_command_and_captures_stdout() {
        let out = run("sh", ["-c", "printf hello"]).unwrap();
        assert_eq!(out.stdout, "hello");
    }

    #[test]
    fn non_zero_exit_becomes_an_error() {
        let err = run("sh", ["-c", "echo boom >&2; exit 3"]).unwrap_err();
        match err {
            Error::ToolFailed { status, stderr, .. } => {
                assert_eq!(status, "exit 3");
                assert!(stderr.contains("boom"));
            }
            other => panic!("expected ToolFailed, got {other:?}"),
        }
    }

    #[test]
    fn missing_tool_error_is_a_not_found() {
        let err = require("kernelctl-not-real", "install it").unwrap_err();
        assert!(err.is_not_found());
        assert_eq!(err.hint().as_deref(), Some("install it"));
    }

    #[test]
    fn probe_reports_success_state() {
        assert!(probe("sh", ["-c", "true"]));
        assert!(!probe("sh", ["-c", "false"]));
    }
}
