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
//! kernelctl - unified kernel and boot configuration management.

mod cli;
mod commands;
mod error;
mod loaders;
mod model;
mod sys;
mod tui;
mod ui;
mod util;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Command};
use commands::App;
use error::Result;
use ui::style;

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Decide the colour policy before anything can print.
    style::init(cli.global.color_override());

    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{} {err}", style::error_prefix());
            if let Some(hint) = err.hint() {
                eprintln!("  {} {}", style::dim("hint:"), style::dim(&hint));
            }
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    // No subcommand means the interactive interface, which is the tool's
    // primary mode.
    let Some(command) = cli.command else {
        let app = App::new(cli.global);
        return tui::run(&app);
    };

    let app = App::new(cli.global);

    match command {
        Command::Status => commands::status::run(&app),
        Command::List { pattern, long } => commands::list::run(&app, pattern.as_deref(), long),
        Command::Loaders => commands::list::loaders(&app),
        Command::Remove { pattern } => commands::set::remove(&app, &pattern),
        Command::SetDefault { pattern } => commands::set::set_default(&app, &pattern),
        Command::SetNext { pattern, clear } => {
            commands::set::set_next(&app, pattern.as_deref(), clear)
        }
        Command::Cmdline { action } => commands::cmdline::run(&app, &action),
        Command::Diff { first, second } => commands::diff::run(&app, &first, &second),
        Command::Timeout { value } => commands::timeout::run(&app, value.as_deref()),
        Command::Clean { keep, list } => commands::clean::run(&app, keep, list),
        Command::Backup { output } => commands::backup::backup(&app, output.as_deref()),
        Command::Restore { archive, list } => commands::backup::restore(&app, &archive, list),
        Command::Help => commands::help::run(&app),
        Command::Tui => tui::run(&app),
    }
}
