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

/// A boot entry as `--json` presents it.
///
/// The struct itself is flattened in unchanged, so every field 1.0 emitted is
/// still there and nothing that reads it breaks. Two additions make it usable
/// without knowing kernelctl's internals: `state` names the flags that were
/// only reachable as bits of an integer, and `built` is the build time as
/// RFC 3339 rather than the serde rendering of a `SystemTime`.
#[derive(serde::Serialize)]
struct JsonEntry<'a> {
    #[serde(flatten)]
    entry: &'a BootEntry,
    state: std::collections::BTreeMap<&'static str, bool>,
    built: Option<String>,
}

fn as_json(entries: &[BootEntry]) -> Vec<JsonEntry<'_>> {
    entries
        .iter()
        .map(|entry| JsonEntry {
            entry,
            state: entry.flags.as_map(),
            built: entry
                .build_time
                .map(|t| crate::util::time::Utc::from_system_time(t).format_rfc3339()),
        })
        .collect()
}

pub fn run(app: &App, pattern: Option<&str>, long: bool) -> Result<()> {
    let mut found = app.entries()?;

    if let Some(p) = pattern {
        found.retain(|e| e.matches(p));
    }

    if app.args.json {
        return print_json(&as_json(&found));
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
            // "add" would be wrong and dangerously so: someone auditing a
            // rescue image needs to know --boot-dir replaces this list, or
            // they will believe they inspected both when they inspected one.
            style::dim(
                "searched: /boot, /efi, /boot/efi, /etc; \
                 pass --boot-dir to search elsewhere instead of these",
            )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EntryFlags, LoaderKind};

    #[test]
    fn json_keeps_every_field_1_0_emitted_and_adds_named_state() {
        let mut entry =
            BootEntry::new(LoaderKind::SystemdBoot, "/boot/loader/a.conf", "a", "Arch Linux");
        entry.flags.insert(EntryFlags::DEFAULT);
        entry.flags.insert(EntryFlags::RUNNING);
        entry.build_time = Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_786_355_956));

        let json = serde_json::to_value(as_json(std::slice::from_ref(&entry))).unwrap();
        let first = &json[0];

        // Anything parsing 1.0 output must keep working, so the original
        // fields have to survive alongside the new ones.
        assert_eq!(first["id"], entry.id.as_str());
        assert!(first["flags"].is_number(), "the raw bitfield was dropped");
        assert!(first["build_time"].is_object(), "the original build_time was dropped");

        // And the additions have to be usable without knowing the bit layout.
        assert_eq!(first["state"]["default"], true);
        assert_eq!(first["state"]["running"], true);
        assert_eq!(first["state"]["broken"], false);
        assert_eq!(first["built"], "2026-08-10T09:59:16Z");
    }

    #[test]
    fn an_entry_without_a_build_time_reports_built_as_null() {
        let entry = BootEntry::new(LoaderKind::EfiStub, "/sys/firmware", "Boot0001", "Firmware");
        let json = serde_json::to_value(as_json(std::slice::from_ref(&entry))).unwrap();
        assert!(json[0]["built"].is_null());
    }
}
