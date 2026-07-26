//! `kernelctl help` - the full help screen.
//!
//! Deliberately more than clap's generated usage: it also documents the TUI
//! keybindings and explains how the program is put together, so a reader can
//! predict what a command will touch before running it.

use crate::error::Result;
use crate::ui::style;

use super::App;

/// Every TUI keybinding, shared with the in-app help overlay so the two can
/// never drift apart.
pub const KEYBINDINGS: &[(&str, &str)] = &[
    ("↑ / k", "move up"),
    ("↓ / j", "move down"),
    ("g / G", "jump to first / last entry"),
    ("PgUp / PgDn", "scroll a page"),
    ("Enter / d", "set the highlighted entry as the permanent default"),
    ("n", "boot the highlighted entry once on the next reboot"),
    ("N", "clear a pending one-shot entry"),
    ("e", "edit the kernel command line of the highlighted entry"),
    ("t", "set the boot menu timeout"),
    ("c", "open the kernel and module cleanup tool"),
    ("b", "write a backup of the bootloader configuration"),
    ("/", "filter entries by pattern"),
    ("Esc", "clear the filter, or close the open modal"),
    ("Tab", "cycle which bootloader's entries are shown"),
    ("r", "re-read all boot configuration from disk"),
    ("? / h", "open this help overlay"),
    ("q", "quit"),
];

const COMMANDS: &[(&str, &str)] = &[
    ("status", "bootloader, architecture, running kernel, default entry, boot space"),
    ("list [PATTERN]", "styled table of every boot entry; --long adds paths and cmdlines"),
    ("loaders", "every bootloader detected, with what each one can change"),
    ("set-default <ID>", "permanently change which entry boots"),
    ("set-next <ID>", "boot an entry once on the next reboot; --clear cancels it"),
    ("cmdline get <ID>", "print an entry's kernel parameters"),
    ("cmdline set <ID> <ARGS>", "replace an entry's kernel parameters"),
    ("cmdline add <ID> <ARGS>", "add parameters, replacing any with the same key"),
    ("cmdline remove <ID> <KEYS>", "remove parameters by name"),
    ("diff <ID> <ID>", "compare two entries, highlighting parameter changes"),
    ("timeout [SECONDS]", "read or set the boot menu timeout; `never` waits for input"),
    ("clean", "remove kernels and modules no boot entry references"),
    ("backup", "archive bootloader configuration to a timestamped .tar.gz"),
    ("restore <FILE>", "restore configuration from a backup archive"),
    ("help", "this screen"),
];

const FLAGS: &[(&str, &str)] = &[
    ("--boot-dir <DIR>", "add a search location; repeatable, wins over auto-discovery"),
    ("--loader <NAME>", "act on a specific bootloader instead of the primary one"),
    ("--all", "include entries from every detected bootloader"),
    ("--json", "machine-readable output"),
    ("--dry-run", "report what would change without writing"),
    ("-y, --yes", "assume yes for confirmation prompts"),
    ("--color <WHEN>", "auto, always or never"),
    ("-v, --verbose", "print extra detail, including every file written"),
];

const ID_HELP: &str = "\
Anywhere <ID> is accepted you may give the entry id shown by `list`, an
unambiguous prefix of it, a kernel version, or part of the entry title. If a
pattern matches more than one entry kernelctl stops and lists them rather than
picking one - choosing the wrong entry is not something you find out about
until the machine reboots.";

const ARCHITECTURE: &str = "\
kernelctl is built in four layers.

  sys/       Everything touching the machine: uname, privileges, mounts and
             free space, external helpers, and the atomic write primitive that
             every configuration change goes through.

  loaders/   One adapter per bootloader, each mapping its own on-disk format
             onto a single normalized boot entry. Adapters never talk to each
             other. Discovery probes them all, each reports a confidence
             score, and the highest scorer becomes the loader that commands
             act on - which is why a machine with a leftover GRUB install
             still behaves sensibly.

  commands/  The CLI verbs. They work only with normalized entries, so a
             command never needs to know which bootloader is installed.

  tui/       The interactive interface, driving exactly the same command layer
             as the CLI, so the two can never diverge in behaviour.

Every configuration write copies the original to a .bak file, writes a
temporary file in the same directory, fsyncs it, renames it over the target,
then fsyncs the directory. A crash at any point leaves either the old file or
the new one, never a truncated one. Before changing which entry boots,
kernelctl verifies the kernel and initramfs it names are actually on disk.";

pub fn run(app: &App) -> Result<()> {
    let _ = app;

    println!("{}", style::heading("kernelctl"));
    println!(
        "  {}\n",
        style::dim("manage kernels and boot entries across whichever bootloader this system uses")
    );

    println!("{}", style::heading("USAGE"));
    println!("  kernelctl [OPTIONS] [COMMAND]");
    println!("  {}\n", style::dim("run with no command to open the interactive interface"));

    section("COMMANDS", COMMANDS);
    section("OPTIONS", FLAGS);

    println!("{}", style::heading("SELECTING AN ENTRY"));
    for line in ID_HELP.lines() {
        println!("  {line}");
    }
    println!();

    section("TUI KEYBINDINGS", KEYBINDINGS);

    println!("{}", style::heading("HOW IT WORKS"));
    for line in ARCHITECTURE.lines() {
        if line.is_empty() {
            println!();
        } else {
            println!("  {line}");
        }
    }
    println!();

    println!("{}", style::heading("EXAMPLES"));
    for (cmd, what) in [
        ("kernelctl list --long", "show every entry with its full paths"),
        ("kernelctl set-default 6.11.0", "boot kernel 6.11.0 from now on"),
        ("kernelctl set-next recovery", "boot the recovery entry once"),
        ("kernelctl cmdline add arch loglevel=7", "add a parameter to one entry"),
        ("kernelctl diff 6.11.0 6.10.0", "see what changed between two kernels"),
        ("kernelctl clean --list", "see what could be removed, removing nothing"),
        ("kernelctl --boot-dir /mnt/boot status", "inspect a mounted rescue image"),
    ] {
        println!("  {}", style::bold(cmd));
        println!("    {}", style::dim(what));
    }

    Ok(())
}

fn section(title: &str, rows: &[(&str, &str)]) {
    println!("{}", style::heading(title));
    let width = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    for (key, description) in rows {
        println!("  {}{:pad$}  {description}", style::bold(key), "", pad = width - key.len());
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_documented_keybinding_has_a_description() {
        for (key, description) in KEYBINDINGS {
            assert!(!key.is_empty());
            assert!(!description.is_empty(), "{key} has no description");
        }
    }

    #[test]
    fn documents_each_required_keybinding() {
        let keys: Vec<&str> = KEYBINDINGS.iter().map(|(k, _)| *k).collect();
        for required in ["Enter / d", "n", "e", "t", "c", "b", "/", "? / h", "q"] {
            assert!(keys.contains(&required), "{required} is not documented");
        }
    }

    #[test]
    fn documents_every_core_command() {
        let text: String = COMMANDS.iter().map(|(c, _)| *c).collect::<Vec<_>>().join(" ");
        for required in
            ["status", "list", "set-default", "set-next", "clean", "backup", "restore", "diff"]
        {
            assert!(text.contains(required), "{required} is not documented");
        }
    }

    #[test]
    fn architecture_overview_covers_each_layer() {
        for layer in ["sys/", "loaders/", "commands/", "tui/"] {
            assert!(ARCHITECTURE.contains(layer), "{layer} is not described");
        }
    }
}
