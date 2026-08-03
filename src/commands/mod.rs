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
//! Command implementations and the context they share.

pub mod backup;
pub mod clean;
pub mod cmdline;
pub mod diff;
pub mod entries;
pub mod help;
pub mod list;
pub mod set;
pub mod status;
pub mod timeout;

use std::io::{IsTerminal, Write};

use crate::cli::GlobalArgs;
use crate::error::{Error, Result};
use crate::loaders::registry::{self, Discovery};
use crate::loaders::{BootRoots, Bootloader, Context};
use crate::sys::{Host, Privileges};
use crate::ui::style;

/// Everything the commands share, built once per invocation.
///
/// Discovery, host probing and privilege detection each cost a handful of
/// syscalls, and several commands need all three, so they are gathered here
/// rather than repeated.
pub struct App {
    pub host: Host,
    pub privileges: Privileges,
    pub roots: BootRoots,
    pub discovery: Discovery,
    pub args: GlobalArgs,
}

impl App {
    pub fn new(args: GlobalArgs) -> App {
        // Discovery silently ignores a path that is not a directory, which is
        // right for the paths it guesses at but wrong for one the user typed:
        // a mistyped --boot-dir would otherwise look exactly like "this system
        // has no bootloader".
        for dir in &args.boot_dirs {
            if !dir.is_dir() {
                warn(&format!("--boot-dir {} is not a directory; ignoring it", dir.display()));
            }
        }

        let roots = BootRoots::discover(&args.boot_dirs);
        let discovery = registry::discover(&roots);
        App {
            host: Host::detect(),
            privileges: Privileges::detect(),
            roots,
            discovery,
            args,
        }
    }

    pub fn context(&self) -> Context<'_> {
        Context {
            host: &self.host,
            privileges: &self.privileges,
            roots: &self.roots,
            dry_run: self.args.dry_run,
        }
    }

    /// The loader commands act on: `--loader` if given, else the primary.
    pub fn loader(&self) -> Result<&dyn Bootloader> {
        match self.args.loader {
            Some(name) => {
                let kind = name.into();
                self.discovery.by_kind(kind).ok_or_else(|| {
                    let found = self.discovery.kinds();
                    let detail = if found.is_empty() {
                        "no bootloaders were detected".to_string()
                    } else {
                        format!(
                            "detected: {}",
                            found.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ")
                        )
                    };
                    Error::validation(format!("{kind} was not found on this system; {detail}"))
                })
            }
            None => self.discovery.primary(),
        }
    }

    /// Entries to operate on, honouring `--all` and `--loader`.
    pub fn entries(&self) -> Result<Vec<crate::model::BootEntry>> {
        let ctx = self.context();
        if self.args.all {
            let (entries, errors) = self.discovery.all_entries(&ctx);
            for (kind, err) in errors {
                warn(&format!("{kind}: {err}"));
            }
            return Ok(entries);
        }

        // With no --loader override this is exactly the primary loader's
        // entries, so defer to discovery rather than repeating the
        // annotate-and-sort step.
        match self.args.loader {
            None => self.discovery.entries(&ctx),
            Some(_) => {
                let loader = self.loader()?;
                let mut entries = loader.entries(&ctx)?;
                crate::loaders::annotate(&mut entries, &self.host);
                crate::loaders::sort_entries(&mut entries);
                Ok(entries)
            }
        }
    }

    /// Resolve a pattern against the current entry set.
    pub fn resolve(&self, pattern: &str) -> Result<crate::model::BootEntry> {
        let entries = self.entries()?;
        registry::resolve(&entries, pattern).cloned()
    }

    /// Ask for confirmation, unless `--yes` was given.
    ///
    /// A non-interactive stdin (a script, a pipe) cannot answer, so the
    /// operation is refused rather than silently assumed - these commands
    /// delete files and rewrite boot configuration.
    pub fn confirm(&self, prompt: &str) -> Result<bool> {
        if self.args.yes {
            return Ok(true);
        }
        if !std::io::stdin().is_terminal() {
            return Err(Error::validation(format!(
                "{prompt} - refusing to assume an answer with no terminal; pass --yes to proceed"
            )));
        }

        print!("{prompt} [y/N] ");
        std::io::stdout().flush().ok();

        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer).map_err(Error::from)?;
        Ok(matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
    }

    /// Print the loader's post-write advice, if it has any.
    pub fn print_note(&self, loader: &dyn Bootloader) {
        if let Some(note) = loader.post_write_note() {
            note_line(&note);
        }
    }

    /// Report what a write touched, including where the backup went.
    pub fn report_writes(&self, outcomes: &[crate::sys::atomic::WriteOutcome]) {
        if self.args.dry_run {
            return;
        }
        for outcome in outcomes {
            if self.args.verbose {
                match &outcome.backup {
                    Some(bak) => println!(
                        "  {} {} (previous saved to {})",
                        style::dim("wrote"),
                        outcome.target.display(),
                        bak.display()
                    ),
                    None => {
                        println!("  {} {}", style::dim("wrote"), outcome.target.display())
                    }
                }
            }
        }
    }
}

/// `ok: ...` - an operation succeeded.
pub fn success(message: &str) {
    println!("{} {message}", style::ok_prefix());
}

/// `warning: ...` on stderr, so it does not pollute piped output.
pub fn warn(message: &str) {
    eprintln!("{} {message}", style::warn_prefix());
}

/// An indented advisory note following a successful operation.
pub fn note_line(message: &str) {
    println!("  {} {}", style::dim("note:"), style::dim(message));
}

/// Print a value as JSON.
pub fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| Error::other(format!("could not serialize output: {e}")))?;
    println!("{text}");
    Ok(())
}

/// Report that a dry run would have made a change.
pub fn dry_run_notice(what: &str) {
    println!("{} would {what}", style::paint(style::Style::BoldYellow, "dry-run:"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::Parser;

    fn args(argv: &[&str]) -> GlobalArgs {
        Cli::try_parse_from(argv).unwrap().global
    }

    #[test]
    fn confirm_returns_true_with_yes() {
        let app = App::new(args(&["kernelctl", "--yes", "clean"]));
        assert!(app.confirm("Remove everything?").unwrap());
    }

    #[test]
    fn confirm_refuses_without_a_terminal() {
        // Under `cargo test` stdin is not a terminal, which is exactly the
        // situation this guard exists for.
        let app = App::new(args(&["kernelctl", "clean"]));
        if !std::io::stdin().is_terminal() {
            let err = app.confirm("Remove everything?").unwrap_err();
            assert!(err.to_string().contains("--yes"));
        }
    }

    #[test]
    fn unknown_loader_selection_lists_what_was_found() {
        let app = App::new(args(&["kernelctl", "--loader", "lilo", "list"]));
        // Either LILO is genuinely present, or the error names the
        // alternatives instead of failing opaquely.
        if app.discovery.by_kind(crate::model::LoaderKind::Lilo).is_none() {
            // `dyn Bootloader` is not Debug, so take the error side directly.
            let err = app.loader().err().expect("selection should fail");
            assert!(err.to_string().contains("was not found"));
        }
    }

    #[test]
    fn app_builds_without_a_bootloader_present() {
        // Building the context must never fail, even on a machine with no
        // detectable bootloader - status still has useful things to say.
        let app = App::new(args(&["kernelctl", "status"]));
        assert!(!app.host.kernel_release.is_empty());
    }

    #[test]
    fn command_parsing_feeds_the_app() {
        let cli = Cli::try_parse_from(["kernelctl", "--all", "list"]).unwrap();
        assert!(matches!(cli.command, Some(Command::List { .. })));
        assert!(App::new(cli.global).args.all);
    }
}
