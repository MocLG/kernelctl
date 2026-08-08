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
//! Interactive interface state and the actions it can perform.
//!
//! Kept free of any rendering or terminal code so the whole state machine -
//! selection, filtering, modals, and every mutating action - can be tested
//! without a terminal.

use crate::commands::{clean, App};
use crate::error::Result;
use crate::loaders::{Bootloader, Capabilities, Timeout};
use crate::model::BootEntry;

use super::input::TextInput;

/// Severity of the transient message shown in the footer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub level: Level,
    pub text: String,
}

/// What an action that succeeded wants the footer to say.
///
/// Carries a level because "written" and "in effect" are not the same thing on
/// every bootloader, and the footer should not claim the stronger one.
#[derive(Debug, Clone)]
pub struct Done {
    pub level: Level,
    pub text: String,
}

impl Done {
    fn ok(text: impl Into<String>) -> Done {
        Done { level: Level::Success, text: text.into() }
    }

    /// Written, but the bootloader has not picked it up yet.
    fn pending(text: impl Into<String>) -> Done {
        Done { level: Level::Warning, text: text.into() }
    }
}

/// Which overlay is open, and its state.
pub enum Modal {
    None,
    Help,
    /// Filtering the entry list. The list updates as the pattern is typed.
    Filter(TextInput),
    /// Editing the highlighted entry's kernel command line.
    EditCmdline { entry_id: String, title: String, input: TextInput },
    /// Setting the boot menu timeout.
    Timeout(TextInput),
    /// Reviewing what cleanup would remove.
    Clean { candidates: Vec<clean::Candidate>, scroll: usize },
    /// A yes/no question that gates a pending action.
    Confirm { prompt: String, action: PendingAction },
}

impl Modal {
    pub fn is_open(&self) -> bool {
        !matches!(self, Modal::None)
    }
}

/// An action deferred until the user confirms it.
#[derive(Debug, Clone)]
pub enum PendingAction {
    RemoveCleanCandidates(Vec<clean::Candidate>),
    Backup,
}

pub struct Tui<'a> {
    pub app: &'a App,
    /// Every entry read from the bootloader.
    pub entries: Vec<BootEntry>,
    /// Indices into `entries` that survive the current filter.
    pub visible: Vec<usize>,
    /// Position within `visible`.
    pub cursor: usize,
    /// First visible row, maintained by the renderer as the viewport scrolls.
    pub scroll: usize,
    pub filter: String,
    pub modal: Modal,
    pub message: Option<Message>,
    pub should_quit: bool,
    /// Show entries from every loader rather than just the primary.
    pub show_all: bool,
}

impl<'a> Tui<'a> {
    pub fn new(app: &'a App) -> Tui<'a> {
        let mut tui = Tui {
            app,
            entries: Vec::new(),
            visible: Vec::new(),
            cursor: 0,
            scroll: 0,
            filter: String::new(),
            modal: Modal::None,
            message: None,
            should_quit: false,
            show_all: app.args.all,
        };
        tui.reload();
        tui
    }

    /// Re-read all boot configuration from disk.
    ///
    /// Called after every mutation so the display always reflects what is
    /// actually on disk rather than what we believe we wrote.
    pub fn reload(&mut self) {
        let ctx = self.app.context();
        let result = if self.show_all {
            let (entries, errors) = self.app.discovery.all_entries(&ctx);
            if let Some((kind, err)) = errors.first() {
                self.warn(format!("{kind}: {err}"));
            }
            Ok(entries)
        } else {
            self.app.entries()
        };

        match result {
            Ok(entries) => self.entries = entries,
            Err(e) => {
                // Keep whatever was previously loaded: a transient read error
                // should not blank the screen.
                self.error(format!("could not read boot entries: {e}"));
            }
        }
        self.apply_filter();
    }

    /// Recompute the visible set, keeping the highlighted entry selected where
    /// possible so a filter keystroke does not move the selection unexpectedly.
    pub fn apply_filter(&mut self) {
        let previous = self.selected().map(|e| e.id.clone());

        self.visible = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| self.filter.is_empty() || e.matches(&self.filter))
            .map(|(i, _)| i)
            .collect();

        self.cursor = previous
            .and_then(|id| self.visible.iter().position(|i| self.entries[*i].id == id))
            .unwrap_or(0);

        self.clamp_cursor();
    }

    fn clamp_cursor(&mut self) {
        if self.visible.is_empty() {
            self.cursor = 0;
        } else if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len() - 1;
        }
    }

    pub fn selected(&self) -> Option<&BootEntry> {
        self.visible.get(self.cursor).and_then(|i| self.entries.get(*i))
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let last = self.visible.len() as isize - 1;
        // Saturating rather than wrapping: holding a key should stop at the
        // end of the list, not loop around past the entry being aimed at.
        self.cursor = (self.cursor as isize + delta).clamp(0, last) as usize;
    }

    pub fn move_to_start(&mut self) {
        self.cursor = 0;
    }

    pub fn move_to_end(&mut self) {
        self.cursor = self.visible.len().saturating_sub(1);
    }

    // ---- messages ------------------------------------------------------

    pub fn info(&mut self, text: impl Into<String>) {
        self.message = Some(Message { level: Level::Info, text: text.into() });
    }

    pub fn success(&mut self, text: impl Into<String>) {
        self.message = Some(Message { level: Level::Success, text: text.into() });
    }

    pub fn warn(&mut self, text: impl Into<String>) {
        self.message = Some(Message { level: Level::Warning, text: text.into() });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.message = Some(Message { level: Level::Error, text: text.into() });
    }

    /// Run an action, reporting the outcome in the footer either way.
    ///
    /// An action that worked can still need reporting as a warning: on GRUB 2
    /// and LILO a write is not a change until their own command has run, and
    /// colouring that green would tell the user something untrue.
    fn attempt(&mut self, what: &str, action: impl FnOnce(&mut Self) -> Result<Done>) {
        match action(self) {
            Ok(done) => {
                match done.level {
                    Level::Warning => self.warn(done.text),
                    _ => self.success(done.text),
                }
                self.reload();
            }
            Err(e) => self.error(format!("{what} failed: {e}")),
        }
    }

    // ---- actions -------------------------------------------------------

    /// Refuse to point the bootloader at something that will not boot.
    ///
    /// Shares its rules with the command line, phrased to fit one footer line.
    /// This used to be a second implementation that had fallen behind - it
    /// accepted a disabled entry the command line refused.
    fn preflight(entry: &BootEntry) -> Result<()> {
        crate::preflight::check_short(entry)
    }

    /// Finish a write the way the command line does.
    ///
    /// GRUB 2 and LILO need a command run before a written change reaches the
    /// boot path. Without this the screen said "default is now X" and the
    /// machine went on booting the old entry, with nothing on screen to say so.
    fn settle(&self, loader: &dyn Bootloader, done: String) -> Result<Done> {
        let Some(command) = loader.pending_activation() else { return Ok(Done::ok(done)) };

        if !self.app.args.apply {
            return Ok(Done::pending(format!(
                "{done} - not in effect until `{command}` runs; restart with --apply \
                 to have kernelctl run it"
            )));
        }

        crate::sys::exec::run(&command.program, &command.args).map_err(|e| {
            crate::error::Error::validation(format!(
                "written, but `{command}` failed so it is not in effect: {e}"
            ))
        })?;
        Ok(Done::ok(format!("{done}; `{command}` run")))
    }

    pub fn set_default(&mut self) {
        let Some(entry) = self.selected().cloned() else { return };
        self.attempt("set-default", |tui| {
            let loader = tui.app.loader()?;
            require(loader, Capabilities::SET_DEFAULT, "changing the default entry")?;
            Self::preflight(&entry)?;
            loader.set_default(&tui.app.context(), &entry)?;
            tui.settle(loader, format!("default is now '{}'", entry.title))
        });
    }

    pub fn set_oneshot(&mut self) {
        let Some(entry) = self.selected().cloned() else { return };
        self.attempt("set-next", |tui| {
            let loader = tui.app.loader()?;
            require(loader, Capabilities::SET_ONESHOT, "one-shot boot entries")?;
            Self::preflight(&entry)?;
            loader.set_oneshot(&tui.app.context(), &entry)?;
            tui.settle(loader, format!("next boot only: '{}'", entry.title))
        });
    }

    pub fn clear_oneshot(&mut self) {
        self.attempt("clearing the one-shot entry", |tui| {
            let loader = tui.app.loader()?;
            require(loader, Capabilities::SET_ONESHOT, "one-shot boot entries")?;
            loader.clear_oneshot(&tui.app.context())?;
            Ok(Done::ok("pending one-shot entry cleared"))
        });
    }

    /// Open the command-line editor for the highlighted entry.
    pub fn open_cmdline_editor(&mut self) {
        let Some(entry) = self.selected().cloned() else { return };

        // Check up front rather than letting the user type an edit that
        // cannot be saved.
        match self.app.loader() {
            Ok(loader) if !loader.capabilities().contains(Capabilities::EDIT_CMDLINE) => {
                self.error(format!(
                    "{} does not support editing kernel parameters",
                    loader.kind().display_name()
                ));
                return;
            }
            Err(e) => {
                self.error(e.to_string());
                return;
            }
            _ => {}
        }

        self.modal = Modal::EditCmdline {
            entry_id: entry.id.clone(),
            title: entry.title.clone(),
            input: TextInput::new(entry.cmdline.clone()),
        };
    }

    /// Save the command line currently being edited.
    pub fn commit_cmdline(&mut self, entry_id: &str, cmdline: String) {
        let Some(entry) = self.entries.iter().find(|e| e.id == entry_id).cloned() else {
            self.error("the entry being edited is no longer present");
            return;
        };
        if entry.cmdline == cmdline {
            self.info("kernel parameters unchanged");
            return;
        }
        self.attempt("cmdline set", |tui| {
            let loader = tui.app.loader()?;
            loader.set_cmdline(&tui.app.context(), &entry, &cmdline)?;
            tui.settle(loader, format!("updated kernel parameters for '{}'", entry.title))
        });
    }

    pub fn open_timeout_editor(&mut self) {
        let current = self
            .app
            .loader()
            .ok()
            .and_then(|l| l.timeout(&self.app.context()).ok().flatten());

        let initial = match current {
            Some(Timeout::Seconds(n)) => n.to_string(),
            Some(Timeout::Immediate) => "0".to_string(),
            Some(Timeout::Indefinite) => "never".to_string(),
            None => String::new(),
        };
        self.modal = Modal::Timeout(TextInput::new(initial));
    }

    pub fn commit_timeout(&mut self, value: String) {
        self.attempt("timeout", |tui| {
            let timeout = Timeout::parse(&value)?;
            let loader = tui.app.loader()?;
            require(loader, Capabilities::TIMEOUT, "menu timeout configuration")?;
            loader.set_timeout(&tui.app.context(), timeout)?;
            tui.settle(loader, format!("boot menu timeout is now {timeout}"))
        });
    }

    /// Gather cleanup candidates and open the review modal.
    pub fn open_clean(&mut self) {
        let ctx = self.app.context();
        let (entries, errors) = self.app.discovery.all_entries(&ctx);

        // An unreadable config might reference anything, so deleting on the
        // strength of a partial picture is not safe.
        if let Some((kind, err)) = errors.first() {
            self.error(format!(
                "cannot clean safely: {kind} configuration is unreadable ({err})"
            ));
            return;
        }

        let candidates = clean::find_candidates(self.app, &entries, 0);
        if candidates.is_empty() {
            self.info("nothing to clean: every installed kernel is in use");
            return;
        }
        self.modal = Modal::Clean { candidates, scroll: 0 };
    }

    /// Ask before deleting anything.
    pub fn confirm_clean(&mut self, candidates: Vec<clean::Candidate>) {
        let files: usize = candidates.iter().map(|c| c.paths.len()).sum();
        let bytes: u64 = candidates.iter().map(|c| c.size).sum();
        self.modal = Modal::Confirm {
            prompt: format!(
                "Remove {files} file{} across {} kernel version{}, freeing {}?",
                if files == 1 { "" } else { "s" },
                candidates.len(),
                if candidates.len() == 1 { "" } else { "s" },
                crate::util::time::format_bytes(bytes)
            ),
            action: PendingAction::RemoveCleanCandidates(candidates),
        };
    }

    pub fn confirm_backup(&mut self) {
        self.modal = Modal::Confirm {
            prompt: "Write a backup of the bootloader configuration?".to_string(),
            action: PendingAction::Backup,
        };
    }

    /// Carry out a confirmed action.
    pub fn perform(&mut self, action: PendingAction) {
        match action {
            PendingAction::RemoveCleanCandidates(candidates) => {
                if let Err(e) = self.app.privileges.require("clean") {
                    self.error(e.to_string());
                    return;
                }
                let mut removed = 0usize;
                let mut freed = 0u64;
                let mut failed = 0usize;

                for candidate in &candidates {
                    for path in &candidate.paths {
                        let size = candidate.size;
                        let result = if path.is_dir() {
                            std::fs::remove_dir_all(path)
                        } else {
                            std::fs::remove_file(path)
                        };
                        match result {
                            Ok(()) => {
                                removed += 1;
                                freed = freed.saturating_add(size);
                            }
                            Err(_) => failed += 1,
                        }
                    }
                }

                if failed > 0 {
                    self.warn(format!("removed {removed} files; {failed} could not be removed"));
                } else {
                    self.success(format!(
                        "removed {removed} file{}, freeing {}",
                        if removed == 1 { "" } else { "s" },
                        crate::util::time::format_bytes(freed)
                    ));
                }
                self.reload();
            }
            PendingAction::Backup => {
                self.attempt("backup", |tui| {
                    crate::commands::backup::backup(tui.app, None)?;
                    Ok(Done::ok("bootloader configuration backed up"))
                });
            }
        }
    }

    /// Cycle between the primary loader's entries and every loader's.
    pub fn toggle_scope(&mut self) {
        self.show_all = !self.show_all;
        self.reload();
        if self.show_all {
            self.info("showing entries from every detected bootloader");
        } else {
            self.info("showing entries from the primary bootloader");
        }
    }
}

fn require(
    loader: &dyn crate::loaders::Bootloader,
    capability: Capabilities,
    action: &str,
) -> Result<()> {
    if loader.capabilities().contains(capability) {
        Ok(())
    } else {
        Err(crate::error::Error::unsupported(loader.kind().display_name(), action))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use crate::sys::Privileges;
    use crate::loaders::testsupport::{fake_kernel, TempTree};
    use clap::Parser;

    /// A scratch ESP plus an App pointed at it.
    fn fixture(tag: &str) -> (TempTree, App) {
        let tree = TempTree::new(tag);
        tree.dir("EFI/systemd");
        tree.file("loader/loader.conf", "default arch.conf\ntimeout 5\n");
        tree.file(
            "loader/entries/arch.conf",
            "title Arch Linux\nversion 6.12.1\nlinux /vmlinuz-linux\noptions root=UUID=abc rw quiet\n",
        );
        tree.file(
            "loader/entries/fallback.conf",
            "title Arch Linux (fallback)\nversion 6.12.1\nlinux /vmlinuz-linux\noptions root=UUID=abc rw single\n",
        );
        tree.file(
            "loader/entries/old.conf",
            "title Older\nversion 6.9.0\nlinux /vmlinuz-old\noptions root=UUID=abc rw\n",
        );
        fake_kernel(&tree, "vmlinuz-linux");
        fake_kernel(&tree, "vmlinuz-old");

        let root = tree.root.display().to_string();
        let cli = Cli::try_parse_from(["kernelctl", "--boot-dir", &root, "tui"]).unwrap();
        let mut app = App::new(cli.global);
        // These tests assert what a write does, so they must supply the
        // privileges rather than inherit whoever is running them - otherwise
        // they silently pass as root and silently fail everywhere else.
        app.privileges = Privileges { root: true, uid: 0, via_sudo: false };
        (tree, app)
    }

    /// A GRUB tree whose menu takes its default from a fixed index, so a write
    /// to grubenv is not yet a change.
    fn grub_fixture(tag: &str) -> (TempTree, App) {
        let tree = TempTree::new(tag);
        tree.file(
            "grub/grub.cfg",
            "set default=\"0\"\n\
             menuentry 'Debian' $menuentry_id_option 'gnulinux-simple-abc' {\n\
             \tlinux /vmlinuz-6.11.0-9-generic root=UUID=abc ro\n\
             \tinitrd /initrd.img-6.11.0-9-generic\n\
             }\n\
             menuentry 'Debian (other)' $menuentry_id_option 'gnulinux-other-abc' {\n\
             \tlinux /vmlinuz-6.11.0-9-generic root=UUID=abc ro single\n\
             \tinitrd /initrd.img-6.11.0-9-generic\n\
             }\n",
        );
        let mut env = String::from(crate::loaders::grubenv::SIGNATURE);
        env.push_str("saved_entry=gnulinux-simple-abc\n");
        env.push_str(&"#".repeat(crate::loaders::grubenv::BLOCK_SIZE - env.len()));
        tree.file("grub/grubenv", &env);
        fake_kernel(&tree, "vmlinuz-6.11.0-9-generic");
        fake_kernel(&tree, "initrd.img-6.11.0-9-generic");

        let root = tree.root.display().to_string();
        let cli = Cli::try_parse_from(["kernelctl", "--boot-dir", &root, "tui"]).unwrap();
        let mut app = App::new(cli.global);
        app.privileges = Privileges { root: true, uid: 0, via_sudo: false };
        (tree, app)
    }

    #[test]
    fn a_change_grub_has_not_picked_up_is_a_warning_not_a_success() {
        // The screen used to report this green and say "default is now X"
        // while the machine went on booting the old entry.
        let (_tree, app) = grub_fixture("tui-grub-pending");
        let mut tui = Tui::new(&app);
        let target =
            tui.visible.iter().position(|i| tui.entries[*i].title.contains("other")).unwrap();
        tui.cursor = target;
        tui.set_default();

        let message = tui.message.as_ref().expect("something was reported");
        assert_eq!(message.level, Level::Warning, "reported as done: {}", message.text);
        assert!(message.text.contains("update-grub"), "does not name the command: {}", message.text);
        assert!(message.text.contains("--apply"), "does not say how to run it: {}", message.text);
    }

    #[test]
    fn loads_entries_on_start() {
        let (_tree, app) = fixture("tui-load");
        let tui = Tui::new(&app);
        assert_eq!(tui.entries.len(), 3);
        assert_eq!(tui.visible.len(), 3);
        assert!(tui.selected().is_some());
    }

    #[test]
    fn selection_stops_at_the_list_ends() {
        let (_tree, app) = fixture("tui-move");
        let mut tui = Tui::new(&app);

        tui.move_by(-1);
        assert_eq!(tui.cursor, 0, "must not wrap past the top");

        tui.move_by(100);
        assert_eq!(tui.cursor, 2, "must not wrap past the bottom");

        tui.move_to_start();
        assert_eq!(tui.cursor, 0);
        tui.move_to_end();
        assert_eq!(tui.cursor, 2);
    }

    #[test]
    fn filtering_narrows_the_visible_list() {
        let (_tree, app) = fixture("tui-filter");
        let mut tui = Tui::new(&app);

        tui.filter = "Older".into();
        tui.apply_filter();
        assert_eq!(tui.visible.len(), 1);
        assert_eq!(tui.selected().unwrap().title, "Older");

        tui.filter.clear();
        tui.apply_filter();
        assert_eq!(tui.visible.len(), 3);
    }

    #[test]
    fn filtering_keeps_the_highlighted_entry_selected() {
        let (_tree, app) = fixture("tui-filter-keep");
        let mut tui = Tui::new(&app);

        tui.move_to_end();
        let selected = tui.selected().unwrap().id.clone();

        // A filter that still matches the selection must not move it.
        tui.filter = "a".into();
        tui.apply_filter();
        if tui.visible.iter().any(|i| tui.entries[*i].id == selected) {
            assert_eq!(tui.selected().unwrap().id, selected);
        }
    }

    #[test]
    fn a_filter_matching_nothing_leaves_no_selection() {
        let (_tree, app) = fixture("tui-filter-empty");
        let mut tui = Tui::new(&app);

        tui.filter = "no-such-entry-anywhere".into();
        tui.apply_filter();
        assert!(tui.visible.is_empty());
        assert!(tui.selected().is_none());
        // Actions on an empty selection must be harmless rather than panic.
        tui.set_default();
        tui.set_oneshot();
    }

    #[test]
    fn set_default_updates_the_config_and_reloads() {
        let (tree, app) = fixture("tui-setdefault");
        let mut tui = Tui::new(&app);

        // Select the entry that is not currently the default.
        let target = tui.visible.iter().position(|i| tui.entries[*i].title == "Older").unwrap();
        tui.cursor = target;
        tui.set_default();

        assert!(tree.read("loader/loader.conf").contains("default old.conf"));
        assert_eq!(tui.message.as_ref().unwrap().level, Level::Success);
        // The reload must reflect the change on disk.
        assert!(tui.entries.iter().find(|e| e.title == "Older").unwrap().is_default());
    }

    #[test]
    fn set_default_refuses_an_entry_with_a_missing_kernel() {
        let (tree, app) = fixture("tui-broken");
        tree.file(
            "loader/entries/broken.conf",
            "title Broken\nversion 6.5.0\nlinux /vmlinuz-absent\noptions root=UUID=abc rw\n",
        );

        let mut tui = Tui::new(&app);
        let target = tui.visible.iter().position(|i| tui.entries[*i].title == "Broken").unwrap();
        tui.cursor = target;
        tui.set_default();

        assert_eq!(tui.message.as_ref().unwrap().level, Level::Error);
        // The config must be untouched.
        assert!(tree.read("loader/loader.conf").contains("default arch.conf"));
    }

    #[test]
    fn set_default_without_root_reports_an_error_and_writes_nothing() {
        let (tree, mut app) = fixture("tui-noroot");
        app.privileges = Privileges { root: false, uid: 1000, via_sudo: false };
        let before = tree.read("loader/loader.conf");

        let mut tui = Tui::new(&app);
        let target = tui.visible.iter().position(|i| tui.entries[*i].title == "Older").unwrap();
        tui.cursor = target;
        tui.set_default();

        assert_eq!(tui.message.as_ref().unwrap().level, Level::Error);
        assert_eq!(tree.read("loader/loader.conf"), before);
    }

    #[test]
    fn cmdline_editor_opens_with_the_current_value() {
        let (_tree, app) = fixture("tui-cmdline-open");
        let mut tui = Tui::new(&app);

        let target = tui.visible.iter().position(|i| tui.entries[*i].title == "Older").unwrap();
        tui.cursor = target;
        tui.open_cmdline_editor();

        match &tui.modal {
            Modal::EditCmdline { input, title, .. } => {
                assert_eq!(input.value(), "root=UUID=abc rw");
                assert_eq!(title, "Older");
            }
            _ => panic!("the editor should be open"),
        }
    }

    #[test]
    fn committing_a_cmdline_writes_it() {
        let (tree, app) = fixture("tui-cmdline-commit");
        let mut tui = Tui::new(&app);

        let id = tui
            .entries
            .iter()
            .find(|e| e.title == "Older")
            .unwrap()
            .id
            .clone();
        tui.commit_cmdline(&id, "root=UUID=abc rw debug".into());

        assert!(tree.read("loader/entries/old.conf").contains("options root=UUID=abc rw debug"));
        assert_eq!(tui.message.as_ref().unwrap().level, Level::Success);
    }

    #[test]
    fn committing_an_unchanged_cmdline_writes_nothing() {
        let (tree, app) = fixture("tui-cmdline-noop");
        let mut tui = Tui::new(&app);
        let before = tree.read("loader/entries/old.conf");

        let entry = tui.entries.iter().find(|e| e.title == "Older").unwrap().clone();
        tui.commit_cmdline(&entry.id, entry.cmdline.clone());

        assert_eq!(tree.read("loader/entries/old.conf"), before);
        assert_eq!(tui.message.as_ref().unwrap().level, Level::Info);
    }

    #[test]
    fn timeout_editor_starts_from_the_configured_value() {
        let (_tree, app) = fixture("tui-timeout-open");
        let mut tui = Tui::new(&app);
        tui.open_timeout_editor();

        match &tui.modal {
            Modal::Timeout(input) => assert_eq!(input.value(), "5"),
            _ => panic!("the timeout editor should be open"),
        }
    }

    #[test]
    fn committing_a_timeout_writes_it() {
        let (tree, app) = fixture("tui-timeout-commit");
        let mut tui = Tui::new(&app);

        tui.commit_timeout("never".into());
        assert!(tree.read("loader/loader.conf").contains("timeout menu-force"));
        assert_eq!(tui.message.as_ref().unwrap().level, Level::Success);
    }

    #[test]
    fn an_invalid_timeout_reports_an_error() {
        let (tree, app) = fixture("tui-timeout-bad");
        let mut tui = Tui::new(&app);
        let before = tree.read("loader/loader.conf");

        tui.commit_timeout("soon".into());

        assert_eq!(tui.message.as_ref().unwrap().level, Level::Error);
        assert_eq!(tree.read("loader/loader.conf"), before);
    }

    #[test]
    fn modal_open_state_is_reported() {
        assert!(!Modal::None.is_open());
        assert!(Modal::Help.is_open());
        assert!(Modal::Filter(TextInput::default()).is_open());
    }
}
