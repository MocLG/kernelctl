//! `kernelctl set-default` and `kernelctl set-next`.

use crate::error::{Error, Result};
use crate::loaders::{Bootloader, Capabilities};
use crate::model::BootEntry;
use crate::ui::style;

use super::{dry_run_notice, success, App};

/// Refuse to point the bootloader at files that are not there.
///
/// A missing kernel is not discovered until the machine fails to boot, at
/// which point the user is at a firmware prompt with no way to undo it. This
/// is the single most valuable check in the program.
fn preflight(entry: &BootEntry) -> Result<()> {
    let missing: Vec<String> = entry
        .referenced_files()
        .iter()
        .filter(|p| p.is_absolute() && !p.exists())
        .map(|p| p.display().to_string())
        .collect();

    if !missing.is_empty() {
        return Err(Error::validation(format!(
            "'{}' references {} that {} not exist:\n  {}\n\
             booting it would drop the machine to a firmware prompt",
            entry.title,
            if missing.len() == 1 { "a file" } else { "files" },
            if missing.len() == 1 { "does" } else { "do" },
            missing.join("\n  ")
        )));
    }

    if entry.flags.contains(crate::model::EntryFlags::SUBMENU) {
        return Err(Error::validation(format!(
            "'{}' is a submenu, not a bootable entry",
            entry.title
        )));
    }

    Ok(())
}

/// Warn where the choice is legal but probably not what was meant.
fn advisories(app: &App, entry: &BootEntry) {
    if entry.flags.contains(crate::model::EntryFlags::FOREIGN_ARCH) {
        super::warn(&format!(
            "'{}' targets {} but this machine is {}",
            entry.title, entry.arch, app.host.arch
        ));
    }
    if entry.flags.contains(crate::model::EntryFlags::RECOVERY) {
        super::warn(&format!("'{}' looks like a recovery entry", entry.title));
    }
}

fn require(loader: &dyn Bootloader, capability: Capabilities, action: &str) -> Result<()> {
    if loader.capabilities().contains(capability) {
        Ok(())
    } else {
        Err(Error::unsupported(loader.kind().display_name(), action))
    }
}

pub fn set_default(app: &App, pattern: &str) -> Result<()> {
    let entry = app.resolve(pattern)?;
    let loader = app.loader()?;

    require(loader, Capabilities::SET_DEFAULT, "changing the default entry")?;
    preflight(&entry)?;
    advisories(app, &entry);

    if entry.is_default() {
        println!("'{}' is already the default", entry.title);
        return Ok(());
    }

    if app.args.dry_run {
        dry_run_notice(&format!("set the default entry to '{}' ({})", entry.title, entry.id));
        return Ok(());
    }

    let outcomes = loader.set_default(&app.context(), &entry)?;

    success(&format!("default boot entry is now {}", style::bold(&entry.title)));
    app.report_writes(&outcomes);
    app.print_note(loader);

    Ok(())
}

pub fn set_next(app: &App, pattern: Option<&str>, clear: bool) -> Result<()> {
    let loader = app.loader()?;
    require(loader, Capabilities::SET_ONESHOT, "one-shot boot entries")?;

    if clear {
        if app.args.dry_run {
            dry_run_notice("clear the pending one-shot boot entry");
            return Ok(());
        }
        let outcomes = loader.clear_oneshot(&app.context())?;
        success("pending one-shot boot entry cleared");
        app.report_writes(&outcomes);
        return Ok(());
    }

    let pattern = pattern.ok_or_else(|| Error::validation("no entry given"))?;
    let entry = app.resolve(pattern)?;

    preflight(&entry)?;
    advisories(app, &entry);

    if app.args.dry_run {
        dry_run_notice(&format!("boot '{}' once on the next reboot", entry.title));
        return Ok(());
    }

    let outcomes = loader.set_oneshot(&app.context(), &entry)?;

    success(&format!("next boot will use {}", style::bold(&entry.title)));
    // The whole point of a one-shot is that it does not persist, so say so.
    super::note_line("this applies to the next boot only; the default is unchanged");
    app.report_writes(&outcomes);
    app.print_note(loader);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EntryFlags, LoaderKind};
    use std::path::PathBuf;

    fn entry_with(kernel: Option<&str>) -> BootEntry {
        let mut e = BootEntry::new(LoaderKind::Grub2, "/boot/grub/grub.cfg", "id", "Linux");
        e.kernel = kernel.map(PathBuf::from);
        e
    }

    #[test]
    fn preflight_rejects_a_missing_kernel() {
        let e = entry_with(Some("/boot/vmlinuz-does-not-exist-anywhere"));
        let err = preflight(&e).unwrap_err();
        // The message has to explain the stakes: this is only discovered at
        // the next boot otherwise.
        assert!(err.to_string().contains("firmware prompt"));
    }

    #[test]
    fn preflight_lists_every_missing_file() {
        let mut e = entry_with(Some("/boot/missing-kernel-xyz"));
        e.initrds = vec![PathBuf::from("/boot/missing-initrd-xyz")];
        let msg = preflight(&e).unwrap_err().to_string();
        assert!(msg.contains("missing-kernel-xyz"));
        assert!(msg.contains("missing-initrd-xyz"));
        assert!(msg.contains("files"), "plural when several are missing");
    }

    #[test]
    fn preflight_accepts_an_entry_whose_files_exist() {
        // /bin/sh is guaranteed present and stands in for a real kernel.
        assert!(preflight(&entry_with(Some("/bin/sh"))).is_ok());
    }

    #[test]
    fn preflight_allows_entries_with_no_files_to_check() {
        // A chainload entry points at another disk and has nothing to stat.
        assert!(preflight(&entry_with(None)).is_ok());
    }

    #[test]
    fn preflight_rejects_a_submenu() {
        let mut e = entry_with(None);
        e.flags.insert(EntryFlags::SUBMENU);
        assert!(preflight(&e).unwrap_err().to_string().contains("submenu"));
    }
}
