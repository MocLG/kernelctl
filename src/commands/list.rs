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
//! `kernelctl list` - the styled boot entry table.

use crate::error::Result;
use crate::model::BootEntry;
use crate::ui::style;
use crate::ui::table::{Column, Table};

use super::{entries, print_json, App};

pub fn run(app: &App, pattern: Option<&str>, long: bool) -> Result<()> {
    let mut found = app.entries()?;

    if let Some(p) = pattern {
        found.retain(|e| e.matches(p));
    }

    if app.args.json {
        return print_json(&found);
    }

    if found.is_empty() {
        match pattern {
            Some(p) => println!("no boot entries match '{p}'"),
            None => println!("no boot entries found"),
        }
        return Ok(());
    }

    if long {
        print_long(&found);
    } else {
        print_table(app, &found);
    }

    Ok(())
}

fn print_table(app: &App, found: &[BootEntry]) {
    // The loader column only earns its space when entries can come from more
    // than one loader.
    let multi_loader = found.iter().map(|e| e.loader).collect::<std::collections::BTreeSet<_>>().len() > 1;

    let mut columns = vec![
        Column::new("id"),
        // The title is long and stays recognisable when cut, so it absorbs a
        // narrow terminal on behalf of the other columns.
        Column::new("title").flexible(3, 12),
        Column::new("version"),
        Column::new("built"),
    ];
    if multi_loader {
        columns.push(Column::new("loader"));
    }
    columns.push(Column::new("state").flexible(1, 9));

    let mut table = Table::new(columns);

    for entry in found {
        let mut row = vec![
            entry.id.clone(),
            entries::indented_title(entry),
            entries::version(entry),
            entries::build_date(entry),
        ];
        if multi_loader {
            row.push(entry.loader.as_str().to_string());
        }
        row.push(entries::badges(entry));
        table.push(row);
    }

    print!("{}", table.render(crate::ui::table::terminal_width()));

    if app.args.verbose {
        println!();
        println!(
            "{}",
            style::dim(&format!(
                "{} entries; run `kernelctl list --long` for paths and command lines",
                found.len()
            ))
        );
    }
}

fn print_long(found: &[BootEntry]) {
    for (i, entry) in found.iter().enumerate() {
        if i > 0 {
            println!();
        }
        let badges = entries::badges(entry);
        let heading = if badges.is_empty() {
            style::heading(&entry.title)
        } else {
            format!("{} {}", style::heading(&entry.title), badges)
        };
        println!("{heading}");
        println!("{}", entries::render_details(entry, "  "));
    }
}

/// `kernelctl loaders` - what was detected, and what each can do.
pub fn loaders(app: &App) -> Result<()> {
    if app.discovery.is_empty() {
        println!("no supported bootloader detected");
        println!(
            "{}",
            style::dim("searched: /boot, /efi, /boot/efi, /etc; pass --boot-dir to add a location")
        );
        return Ok(());
    }

    if app.args.json {
        #[derive(serde::Serialize)]
        struct LoaderInfo {
            kind: crate::model::LoaderKind,
            name: &'static str,
            confidence: u8,
            primary: bool,
            capabilities: Vec<&'static str>,
            config_files: Vec<String>,
        }

        let infos: Vec<LoaderInfo> = app
            .discovery
            .loaders
            .iter()
            .enumerate()
            .map(|(i, l)| LoaderInfo {
                kind: l.kind(),
                name: l.kind().display_name(),
                confidence: l.confidence(),
                primary: i == 0,
                capabilities: l.capabilities().names(),
                config_files: l.config_files().iter().map(|p| p.display().to_string()).collect(),
            })
            .collect();
        return print_json(&infos);
    }

    let mut table = Table::new(vec![
        Column::new("loader"),
        Column::new("confidence").right(),
        Column::new("role"),
        Column::new("capabilities").flexible(2, 12),
    ]);

    for (i, loader) in app.discovery.loaders.iter().enumerate() {
        let caps = loader.capabilities().names();
        table.push(vec![
            loader.kind().display_name().to_string(),
            format!("{}%", loader.confidence()),
            if i == 0 {
                style::paint(style::Style::BoldGreen, "primary")
            } else {
                style::dim("also present")
            },
            if caps.is_empty() { style::dim("read-only") } else { caps.join(", ") },
        ]);
    }

    print!("{}", table.render(crate::ui::table::terminal_width()));

    if app.discovery.loaders.len() > 1 {
        println!();
        println!(
            "{}",
            style::dim(
                "the primary loader is used by default; target another with --loader, \
                 or list every entry with --all"
            )
        );
    }

    Ok(())
}
