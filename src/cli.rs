//! Command-line interface definition.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::model::LoaderKind;

/// Unified kernel and boot configuration management.
#[derive(Debug, Parser)]
#[command(
    name = "kernelctl",
    version,
    about = "Manage kernels and boot entries across whichever bootloader this system uses",
    long_about = None,
    // The interactive TUI is the no-argument behaviour, so clap must not
    // print help and exit when there is no subcommand.
    arg_required_else_help = false,
    disable_help_subcommand = true,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[command(flatten)]
    pub global: GlobalArgs,
}

#[derive(Debug, Args, Clone)]
pub struct GlobalArgs {
    /// Additional directory to search for boot configuration. Repeatable, and
    /// takes priority over auto-discovery - use it to inspect a mounted ESP or
    /// a rescue image.
    #[arg(long = "boot-dir", value_name = "DIR", global = true)]
    pub boot_dirs: Vec<PathBuf>,

    /// Act on a specific bootloader instead of the highest-confidence one.
    #[arg(long, value_name = "LOADER", global = true)]
    pub loader: Option<LoaderName>,

    /// Include entries from every detected bootloader, not just the primary.
    #[arg(long, global = true)]
    pub all: bool,

    /// Emit machine-readable JSON instead of a table.
    #[arg(long, global = true)]
    pub json: bool,

    /// Report what would change without writing anything.
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Assume yes for confirmation prompts.
    #[arg(short = 'y', long, global = true)]
    pub yes: bool,

    /// When to colourize output.
    #[arg(long, value_name = "WHEN", default_value = "auto", global = true)]
    pub color: ColorChoice,

    /// Disable colour. Equivalent to --color=never.
    #[arg(long, global = true, conflicts_with = "color")]
    pub no_color: bool,

    /// Print extra detail.
    #[arg(short, long, global = true)]
    pub verbose: bool,
}

impl GlobalArgs {
    /// Resolve the colour policy: `None` means "decide from the environment".
    pub fn color_override(&self) -> Option<bool> {
        if self.no_color {
            return Some(false);
        }
        match self.color {
            ColorChoice::Auto => None,
            ColorChoice::Always => Some(true),
            ColorChoice::Never => Some(false),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

/// Bootloaders selectable with `--loader`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum LoaderName {
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

impl From<LoaderName> for LoaderKind {
    fn from(n: LoaderName) -> LoaderKind {
        match n {
            LoaderName::Grub2 => LoaderKind::Grub2,
            LoaderName::GrubLegacy => LoaderKind::GrubLegacy,
            LoaderName::SystemdBoot => LoaderKind::SystemdBoot,
            LoaderName::Limine => LoaderKind::Limine,
            LoaderName::Extlinux => LoaderKind::Extlinux,
            LoaderName::Syslinux => LoaderKind::Syslinux,
            LoaderName::Refind => LoaderKind::Refind,
            LoaderName::Lilo => LoaderKind::Lilo,
            LoaderName::EfiStub => LoaderKind::EfiStub,
            LoaderName::Barebox => LoaderKind::Barebox,
            LoaderName::Uki => LoaderKind::Uki,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show the detected bootloader, architecture, running kernel, default
    /// entry and boot partition space.
    Status,

    /// List every detected boot entry.
    #[command(visible_alias = "ls")]
    List {
        /// Only show entries matching this pattern.
        #[arg(value_name = "PATTERN")]
        pattern: Option<String>,

        /// Show resolved paths and command lines for each entry.
        #[arg(short, long)]
        long: bool,
    },

    /// Permanently set the default boot entry.
    SetDefault {
        /// Entry id, kernel version, or part of a title.
        #[arg(value_name = "ID-OR-PATTERN")]
        pattern: String,
    },

    /// Boot an entry once on the next reboot, then revert.
    SetNext {
        /// Entry id, kernel version, or part of a title.
        #[arg(value_name = "ID-OR-PATTERN", required_unless_present = "clear")]
        pattern: Option<String>,

        /// Cancel a pending one-shot entry.
        #[arg(long, conflicts_with = "pattern")]
        clear: bool,
    },

    /// Read or write kernel command-line parameters.
    Cmdline {
        #[command(subcommand)]
        action: CmdlineAction,
    },

    /// Compare two boot entries.
    Diff {
        #[arg(value_name = "ID-OR-PATTERN")]
        first: String,
        #[arg(value_name = "ID-OR-PATTERN")]
        second: String,
    },

    /// Read or write the boot menu timeout.
    Timeout {
        /// Seconds to wait, `0` for no menu, or `never` to wait for input.
        /// Omit to read the current value.
        #[arg(value_name = "SECONDS")]
        value: Option<String>,
    },

    /// Find and remove kernels, initramfs images and module directories that
    /// no boot entry references.
    Clean {
        /// Keep this many of the newest unreferenced kernels.
        #[arg(long, value_name = "N", default_value_t = 0)]
        keep: usize,

        /// List what would be removed without removing anything.
        #[arg(long)]
        list: bool,
    },

    /// Archive bootloader configuration and /boot metadata.
    Backup {
        /// Where to write the archive. Defaults to a timestamped file in the
        /// current directory.
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,
    },

    /// Restore bootloader configuration from a backup archive.
    Restore {
        #[arg(value_name = "BACKUP-FILE")]
        archive: PathBuf,

        /// Show what the archive contains without writing anything.
        #[arg(long)]
        list: bool,
    },

    /// Show every bootloader detected on this system.
    Loaders,

    /// Launch the interactive terminal interface.
    Tui,

    /// Show the full help screen, including keybindings and an architectural
    /// overview.
    Help,
}

#[derive(Debug, Subcommand)]
pub enum CmdlineAction {
    /// Print an entry's kernel parameters.
    Get {
        #[arg(value_name = "ID-OR-PATTERN")]
        pattern: String,

        /// Print one parameter per line instead of the raw command line.
        #[arg(long)]
        split: bool,
    },

    /// Replace an entry's kernel parameters.
    Set {
        #[arg(value_name = "ID-OR-PATTERN")]
        pattern: String,

        #[arg(value_name = "ARGS")]
        args: String,
    },

    /// Add parameters to an entry, replacing any with the same key.
    Add {
        #[arg(value_name = "ID-OR-PATTERN")]
        pattern: String,

        #[arg(value_name = "ARGS", required = true, num_args = 1..)]
        args: Vec<String>,
    },

    /// Remove parameters from an entry by name.
    Remove {
        #[arg(value_name = "ID-OR-PATTERN")]
        pattern: String,

        #[arg(value_name = "KEYS", required = true, num_args = 1..)]
        keys: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        // Catches conflicting argument names and bad defaults at test time
        // rather than on first run.
        Cli::command().debug_assert();
    }

    #[test]
    fn no_arguments_means_the_tui() {
        let cli = Cli::try_parse_from(["kernelctl"]).unwrap();
        assert!(cli.command.is_none());
    }

    #[test]
    fn parses_set_default() {
        let cli = Cli::try_parse_from(["kernelctl", "set-default", "6.11.0"]).unwrap();
        match cli.command {
            Some(Command::SetDefault { pattern }) => assert_eq!(pattern, "6.11.0"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn set_next_requires_a_pattern_or_clear() {
        assert!(Cli::try_parse_from(["kernelctl", "set-next"]).is_err());
        assert!(Cli::try_parse_from(["kernelctl", "set-next", "--clear"]).is_ok());
        assert!(Cli::try_parse_from(["kernelctl", "set-next", "arch"]).is_ok());
        // Naming an entry and clearing at once is contradictory.
        assert!(Cli::try_parse_from(["kernelctl", "set-next", "arch", "--clear"]).is_err());
    }

    #[test]
    fn parses_cmdline_subcommands() {
        let cli =
            Cli::try_parse_from(["kernelctl", "cmdline", "set", "arch", "root=/dev/sda1 ro"])
                .unwrap();
        match cli.command {
            Some(Command::Cmdline { action: CmdlineAction::Set { pattern, args } }) => {
                assert_eq!(pattern, "arch");
                assert_eq!(args, "root=/dev/sda1 ro");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn global_flags_work_after_the_subcommand() {
        let cli = Cli::try_parse_from(["kernelctl", "list", "--json", "--all"]).unwrap();
        assert!(cli.global.json);
        assert!(cli.global.all);
    }

    #[test]
    fn boot_dir_is_repeatable() {
        let cli =
            Cli::try_parse_from(["kernelctl", "--boot-dir", "/mnt/a", "--boot-dir", "/mnt/b", "status"])
                .unwrap();
        assert_eq!(cli.global.boot_dirs.len(), 2);
    }

    #[test]
    fn colour_policy_resolves() {
        let cli = Cli::try_parse_from(["kernelctl", "list"]).unwrap();
        assert_eq!(cli.global.color_override(), None, "auto defers to the environment");

        let cli = Cli::try_parse_from(["kernelctl", "--no-color", "list"]).unwrap();
        assert_eq!(cli.global.color_override(), Some(false));

        let cli = Cli::try_parse_from(["kernelctl", "--color", "always", "list"]).unwrap();
        assert_eq!(cli.global.color_override(), Some(true));
    }

    #[test]
    fn loader_names_map_onto_loader_kinds() {
        assert_eq!(LoaderKind::from(LoaderName::SystemdBoot), LoaderKind::SystemdBoot);
        let cli =
            Cli::try_parse_from(["kernelctl", "--loader", "systemd-boot", "list"]).unwrap();
        assert_eq!(cli.global.loader, Some(LoaderName::SystemdBoot));
    }

    #[test]
    fn list_has_a_short_alias() {
        assert!(Cli::try_parse_from(["kernelctl", "ls"]).is_ok());
    }
}
