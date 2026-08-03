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
//! The interactive terminal interface.
//!
//! Every action here goes through the same loader trait the CLI uses, so the
//! two cannot drift apart in behaviour - the TUI is a different way to drive
//! the same operations, not a reimplementation of them.

mod input;
mod render;
mod state;

use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::commands::App;
use crate::error::{Error, Result};

use input::TextInput;
use state::{Modal, Tui};

type Backend = CrosstermBackend<Stdout>;

/// How long to wait for a key before redrawing anyway.
///
/// Long enough that an idle interface costs nothing, short enough that a
/// terminal resize is picked up without a visible lag.
const TICK: Duration = Duration::from_millis(250);

/// Run the interactive interface.
pub fn run(app: &App) -> Result<()> {
    if !std::io::IsTerminal::is_terminal(&io::stdout()) {
        return Err(Error::validation(
            "the interactive interface needs a terminal; use `kernelctl list` when piping output",
        ));
    }

    let mut terminal = setup()?;
    // The terminal is in raw mode with an alternate screen active, so a panic
    // from here on would otherwise leave the user with an unusable shell.
    install_panic_hook();

    let result = event_loop(&mut terminal, app);

    // Restore before reporting, so an error message is printed to a sane
    // terminal.
    restore(&mut terminal);
    result
}

fn setup() -> Result<Terminal<Backend>> {
    enable_raw_mode().map_err(|e| Error::other(format!("could not enter raw mode: {e}")))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)
        .map_err(|e| Error::other(format!("could not open the alternate screen: {e}")))?;
    Terminal::new(CrosstermBackend::new(stdout))
        .map_err(|e| Error::other(format!("could not initialize the terminal: {e}")))
}

fn restore(terminal: &mut Terminal<Backend>) {
    // Best effort throughout: if restoring fails there is nothing useful left
    // to do about it, and the original error matters more.
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();
}

/// Restore the terminal before a panic message is printed.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original(info);
    }));
}

fn event_loop(terminal: &mut Terminal<Backend>, app: &App) -> Result<()> {
    let mut tui = Tui::new(app);

    if tui.entries.is_empty() {
        tui.warn("no boot entries found - press ? for help, r to re-read");
    }

    while !tui.should_quit {
        terminal
            .draw(|frame| render::draw(frame, &mut tui))
            .map_err(|e| Error::other(format!("could not draw the interface: {e}")))?;

        if !event::poll(TICK).map_err(|e| Error::other(format!("input failed: {e}")))? {
            continue;
        }

        match event::read().map_err(|e| Error::other(format!("input failed: {e}")))? {
            // Windows terminals report both press and release; acting on both
            // would run every action twice.
            Event::Key(key) if key.kind == KeyEventKind::Press => handle_key(&mut tui, key),
            Event::Resize(_, _) => {}
            _ => {}
        }
    }

    Ok(())
}

fn handle_key(tui: &mut Tui, key: KeyEvent) {
    // ctrl-c always quits, whatever is open.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        tui.should_quit = true;
        return;
    }

    if tui.modal.is_open() {
        handle_modal_key(tui, key);
        return;
    }

    // A new keystroke supersedes the last action's message.
    tui.message = None;

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => {
            // Esc clears an active filter before it quits, so it never
            // discards work unexpectedly.
            if tui.filter.is_empty() {
                tui.should_quit = true;
            } else {
                tui.filter.clear();
                tui.apply_filter();
            }
        }
        KeyCode::Up | KeyCode::Char('k') => tui.move_by(-1),
        KeyCode::Down | KeyCode::Char('j') => tui.move_by(1),
        KeyCode::PageUp => tui.move_by(-10),
        KeyCode::PageDown => tui.move_by(10),
        KeyCode::Home | KeyCode::Char('g') => tui.move_to_start(),
        KeyCode::End | KeyCode::Char('G') => tui.move_to_end(),

        KeyCode::Enter | KeyCode::Char('d') => tui.set_default(),
        KeyCode::Char('n') => tui.set_oneshot(),
        KeyCode::Char('N') => tui.clear_oneshot(),
        KeyCode::Char('e') => tui.open_cmdline_editor(),
        KeyCode::Char('t') => tui.open_timeout_editor(),
        KeyCode::Char('c') => tui.open_clean(),
        KeyCode::Char('b') => tui.confirm_backup(),
        KeyCode::Char('/') => tui.modal = Modal::Filter(TextInput::new(tui.filter.clone())),
        KeyCode::Tab => tui.toggle_scope(),
        KeyCode::Char('r') => {
            tui.reload();
            tui.info("re-read boot configuration from disk");
        }
        KeyCode::Char('?') | KeyCode::Char('h') => tui.modal = Modal::Help,
        _ => {}
    }
}

fn handle_modal_key(tui: &mut Tui, key: KeyEvent) {
    // Take the modal so its state can be moved out and edited freely; each
    // branch either puts it back or leaves it closed.
    let modal = std::mem::replace(&mut tui.modal, Modal::None);

    match modal {
        Modal::None => {}

        // Any key dismisses help.
        Modal::Help => {}

        Modal::Filter(mut field) => match key.code {
            KeyCode::Enter => {
                tui.filter = field.into_value();
                tui.apply_filter();
            }
            KeyCode::Esc => {
                // Esc abandons the filter entirely rather than keeping a
                // half-typed one.
                tui.filter.clear();
                tui.apply_filter();
            }
            _ => {
                edit(&mut field, key);
                // Filtering live makes the list respond as the user types.
                tui.filter = field.value().to_string();
                tui.apply_filter();
                tui.modal = Modal::Filter(field);
            }
        },

        Modal::EditCmdline { entry_id, title, input: mut field } => match key.code {
            KeyCode::Enter => tui.commit_cmdline(&entry_id, field.into_value()),
            KeyCode::Esc => tui.info("edit cancelled"),
            _ => {
                edit(&mut field, key);
                tui.modal = Modal::EditCmdline { entry_id, title, input: field };
            }
        },

        Modal::Timeout(mut field) => match key.code {
            KeyCode::Enter => tui.commit_timeout(field.into_value()),
            KeyCode::Esc => {}
            _ => {
                edit(&mut field, key);
                tui.modal = Modal::Timeout(field);
            }
        },

        Modal::Clean { candidates, scroll } => match key.code {
            KeyCode::Enter => tui.confirm_clean(candidates),
            KeyCode::Esc | KeyCode::Char('q') => {}
            KeyCode::Up | KeyCode::Char('k') => {
                tui.modal = Modal::Clean { candidates, scroll: scroll.saturating_sub(1) };
            }
            KeyCode::Down | KeyCode::Char('j') => {
                tui.modal = Modal::Clean { candidates, scroll: scroll + 1 };
            }
            _ => tui.modal = Modal::Clean { candidates, scroll },
        },

        Modal::Confirm { prompt, action } => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => tui.perform(action),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => tui.info("cancelled"),
            // Anything else leaves the question on screen rather than guessing.
            _ => tui.modal = Modal::Confirm { prompt, action },
        },
    }
}

/// Apply a keystroke to a text field.
fn edit(field: &mut TextInput, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match key.code {
        // The readline bindings people already have in their fingers.
        KeyCode::Char('w') if ctrl => field.delete_word(),
        KeyCode::Char('u') if ctrl => field.delete_to_start(),
        KeyCode::Char('k') if ctrl => field.delete_to_end(),
        KeyCode::Char('a') if ctrl => field.home(),
        KeyCode::Char('e') if ctrl => field.end(),
        KeyCode::Char(c) if !ctrl => field.insert(c),
        KeyCode::Backspace => field.backspace(),
        KeyCode::Delete => field.delete(),
        KeyCode::Left => field.left(),
        KeyCode::Right => field.right(),
        KeyCode::Home => field.home(),
        KeyCode::End => field.end(),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use crate::loaders::testsupport::{fake_kernel, TempTree};
    use clap::Parser;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn fixture(tag: &str) -> (TempTree, App) {
        let tree = TempTree::new(tag);
        tree.dir("EFI/systemd");
        tree.file("loader/loader.conf", "default arch.conf\ntimeout 5\n");
        tree.file(
            "loader/entries/arch.conf",
            "title Arch Linux\nversion 6.12.1\nlinux /vmlinuz-linux\noptions root=UUID=abc rw\n",
        );
        tree.file(
            "loader/entries/old.conf",
            "title Older\nversion 6.9.0\nlinux /vmlinuz-old\noptions root=UUID=abc rw\n",
        );
        fake_kernel(&tree, "vmlinuz-linux");
        fake_kernel(&tree, "vmlinuz-old");

        let root = tree.root.display().to_string();
        let cli = Cli::try_parse_from(["kernelctl", "--boot-dir", &root, "tui"]).unwrap();
        (tree, App::new(cli.global))
    }

    #[test]
    fn q_quits_and_esc_clears_a_filter_first() {
        let (_tree, app) = fixture("tui-quit");
        let mut tui = Tui::new(&app);

        tui.filter = "Older".into();
        tui.apply_filter();

        // Esc with a filter active clears it rather than quitting, so a
        // reflexive Esc never throws away the session.
        handle_key(&mut tui, press(KeyCode::Esc));
        assert!(!tui.should_quit);
        assert!(tui.filter.is_empty());

        handle_key(&mut tui, press(KeyCode::Esc));
        assert!(tui.should_quit);
    }

    #[test]
    fn ctrl_c_quits_even_with_a_modal_open() {
        let (_tree, app) = fixture("tui-ctrlc");
        let mut tui = Tui::new(&app);
        tui.modal = Modal::Help;

        handle_key(&mut tui, ctrl('c'));
        assert!(tui.should_quit);
    }

    #[test]
    fn navigation_keys_move_the_selection() {
        let (_tree, app) = fixture("tui-nav");
        let mut tui = Tui::new(&app);

        handle_key(&mut tui, press(KeyCode::Down));
        assert_eq!(tui.cursor, 1);
        handle_key(&mut tui, press(KeyCode::Char('k')));
        assert_eq!(tui.cursor, 0);
        handle_key(&mut tui, press(KeyCode::Char('G')));
        assert_eq!(tui.cursor, tui.visible.len() - 1);
    }

    #[test]
    fn question_mark_opens_help_and_any_key_closes_it() {
        let (_tree, app) = fixture("tui-help");
        let mut tui = Tui::new(&app);

        handle_key(&mut tui, press(KeyCode::Char('?')));
        assert!(matches!(tui.modal, Modal::Help));

        handle_key(&mut tui, press(KeyCode::Char('x')));
        assert!(!tui.modal.is_open());
    }

    #[test]
    fn slash_opens_the_filter_and_typing_narrows_the_list() {
        let (_tree, app) = fixture("tui-filter-key");
        let mut tui = Tui::new(&app);
        assert_eq!(tui.visible.len(), 2);

        handle_key(&mut tui, press(KeyCode::Char('/')));
        assert!(tui.modal.is_open());

        for c in "Older".chars() {
            handle_key(&mut tui, press(KeyCode::Char(c)));
        }
        // The list narrows as it is typed, before Enter is pressed.
        assert_eq!(tui.visible.len(), 1);

        handle_key(&mut tui, press(KeyCode::Enter));
        assert!(!tui.modal.is_open());
        assert_eq!(tui.filter, "Older");
    }

    #[test]
    fn escaping_the_filter_restores_the_full_list() {
        let (_tree, app) = fixture("tui-filter-esc");
        let mut tui = Tui::new(&app);

        handle_key(&mut tui, press(KeyCode::Char('/')));
        handle_key(&mut tui, press(KeyCode::Char('O')));
        handle_key(&mut tui, press(KeyCode::Esc));

        assert!(tui.filter.is_empty());
        assert_eq!(tui.visible.len(), 2);
    }

    #[test]
    fn letters_type_into_a_modal_instead_of_triggering_shortcuts() {
        let (tree, app) = fixture("tui-capture");
        let mut tui = Tui::new(&app);
        let before = tree.read("loader/loader.conf");

        handle_key(&mut tui, press(KeyCode::Char('/')));
        // 'd' is the set-default shortcut; inside a text field it must be text.
        handle_key(&mut tui, press(KeyCode::Char('d')));

        assert_eq!(tree.read("loader/loader.conf"), before);
        assert_eq!(tui.filter, "d");
    }

    #[test]
    fn e_opens_the_cmdline_editor_and_enter_saves() {
        let (tree, app) = fixture("tui-edit");
        let mut tui = Tui::new(&app);

        let target = tui.visible.iter().position(|i| tui.entries[*i].title == "Older").unwrap();
        tui.cursor = target;

        handle_key(&mut tui, press(KeyCode::Char('e')));
        assert!(tui.modal.is_open());

        for c in " debug".chars() {
            handle_key(&mut tui, press(KeyCode::Char(c)));
        }
        handle_key(&mut tui, press(KeyCode::Enter));

        assert!(tree.read("loader/entries/old.conf").contains("options root=UUID=abc rw debug"));
    }

    #[test]
    fn escaping_the_cmdline_editor_discards_the_edit() {
        let (tree, app) = fixture("tui-edit-esc");
        let mut tui = Tui::new(&app);
        let before = tree.read("loader/entries/old.conf");

        handle_key(&mut tui, press(KeyCode::Char('e')));
        handle_key(&mut tui, press(KeyCode::Char('X')));
        handle_key(&mut tui, press(KeyCode::Esc));

        assert!(!tui.modal.is_open());
        assert_eq!(tree.read("loader/entries/old.conf"), before);
    }

    #[test]
    fn ctrl_w_deletes_a_word_in_the_editor() {
        let (_tree, app) = fixture("tui-edit-ctrlw");
        let mut tui = Tui::new(&app);

        handle_key(&mut tui, press(KeyCode::Char('e')));
        handle_key(&mut tui, ctrl('w'));

        match &tui.modal {
            Modal::EditCmdline { input, .. } => {
                assert_eq!(input.value(), "root=UUID=abc ");
            }
            _ => panic!("the editor should still be open"),
        }
    }

    #[test]
    fn d_sets_the_default_entry() {
        let (tree, app) = fixture("tui-setdefault-key");
        let mut tui = Tui::new(&app);

        let target = tui.visible.iter().position(|i| tui.entries[*i].title == "Older").unwrap();
        tui.cursor = target;
        handle_key(&mut tui, press(KeyCode::Char('d')));

        assert!(tree.read("loader/loader.conf").contains("default old.conf"));
    }

    #[test]
    fn t_opens_the_timeout_editor_prefilled() {
        let (tree, app) = fixture("tui-timeout-key");
        let mut tui = Tui::new(&app);

        handle_key(&mut tui, press(KeyCode::Char('t')));
        handle_key(&mut tui, ctrl('u'));
        for c in "20".chars() {
            handle_key(&mut tui, press(KeyCode::Char(c)));
        }
        handle_key(&mut tui, press(KeyCode::Enter));

        assert!(tree.read("loader/loader.conf").contains("timeout 20"));
    }

    #[test]
    fn b_asks_before_writing_a_backup() {
        let (_tree, app) = fixture("tui-backup-key");
        let mut tui = Tui::new(&app);

        handle_key(&mut tui, press(KeyCode::Char('b')));
        assert!(matches!(tui.modal, Modal::Confirm { .. }));

        // A destructive-ish action must not proceed on an unrelated keystroke.
        handle_key(&mut tui, press(KeyCode::Char('z')));
        assert!(matches!(tui.modal, Modal::Confirm { .. }));

        handle_key(&mut tui, press(KeyCode::Char('n')));
        assert!(!tui.modal.is_open());
    }

    #[test]
    fn r_reloads_from_disk() {
        let (tree, app) = fixture("tui-reload");
        let mut tui = Tui::new(&app);
        assert_eq!(tui.entries.len(), 2);

        tree.file("loader/entries/third.conf", "title Third\nlinux /vmlinuz-linux\n");
        handle_key(&mut tui, press(KeyCode::Char('r')));

        assert_eq!(tui.entries.len(), 3);
    }
}
