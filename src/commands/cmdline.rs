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
//! `kernelctl cmdline` - read and write kernel parameters without an editor.

use crate::cli::CmdlineAction;
use crate::error::{Error, Result};
use crate::loaders::Capabilities;
use crate::model::{split_cmdline, BootEntry};
use crate::ui::style;

use super::{dry_run_notice, success, App};

pub fn run(app: &App, action: &CmdlineAction) -> Result<()> {
    match action {
        CmdlineAction::Get { pattern, split } => get(app, pattern, *split),
        CmdlineAction::Set { pattern, args } => set(app, pattern, args),
        CmdlineAction::Add { pattern, args } => add(app, pattern, args),
        CmdlineAction::Remove { pattern, keys } => remove(app, pattern, keys),
    }
}

fn get(app: &App, pattern: &str, split: bool) -> Result<()> {
    let entry = app.resolve(pattern)?;

    if app.args.json {
        #[derive(serde::Serialize)]
        struct Out<'a> {
            id: &'a str,
            title: &'a str,
            cmdline: &'a str,
            parameters: Vec<String>,
        }
        return super::print_json(&Out {
            id: &entry.id,
            title: &entry.title,
            cmdline: &entry.cmdline,
            parameters: entry.cmdline_params(),
        });
    }

    if split {
        for param in entry.cmdline_params() {
            println!("{param}");
        }
    } else {
        // Bare output with no decoration, so it can be captured directly:
        //   kernelctl cmdline get arch | xargs ...
        println!("{}", entry.cmdline);
    }
    Ok(())
}

fn set(app: &App, pattern: &str, args: &str) -> Result<()> {
    let entry = app.resolve(pattern)?;
    apply(app, &entry, args.trim())
}

fn add(app: &App, pattern: &str, args: &[String]) -> Result<()> {
    let entry = app.resolve(pattern)?;
    let updated = merge_params(&entry.cmdline, args);
    apply(app, &entry, &updated)
}

fn remove(app: &App, pattern: &str, keys: &[String]) -> Result<()> {
    let entry = app.resolve(pattern)?;
    let updated = remove_params(&entry.cmdline, keys);
    if updated == entry.cmdline {
        println!("no matching parameters on '{}'", entry.title);
        return Ok(());
    }
    apply(app, &entry, &updated)
}

/// Add or replace parameters.
///
/// A `key=value` addition replaces any existing parameter with the same key
/// rather than appending a duplicate: the kernel takes the last occurrence,
/// so a duplicate would work but leave the config confusing to read.
pub fn merge_params(cmdline: &str, additions: &[String]) -> String {
    let mut params = split_cmdline(cmdline);

    for addition in additions {
        // A single argument may itself carry several parameters, as when the
        // shell passed them in one quoted string.
        for new in split_cmdline(addition) {
            let key = param_key(&new);
            match params.iter_mut().find(|p| param_key(p) == key) {
                Some(existing) => *existing = new,
                None => params.push(new),
            }
        }
    }

    params.join(" ")
}

/// Remove parameters by name. A bare flag is matched by its own name.
pub fn remove_params(cmdline: &str, keys: &[String]) -> String {
    let wanted: Vec<&str> = keys.iter().map(|k| param_key(k)).collect();
    split_cmdline(cmdline)
        .into_iter()
        .filter(|p| !wanted.contains(&param_key(p)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The name part of a parameter: everything before the first `=`.
fn param_key(param: &str) -> &str {
    param.split_once('=').map(|(k, _)| k).unwrap_or(param)
}

fn apply(app: &App, entry: &BootEntry, cmdline: &str) -> Result<()> {
    let loader = app.loader()?;
    if !loader.capabilities().contains(Capabilities::EDIT_CMDLINE) {
        return Err(Error::unsupported(
            loader.kind().display_name(),
            "editing kernel parameters",
        ));
    }

    if cmdline == entry.cmdline {
        println!("'{}' already has that command line", entry.title);
        return Ok(());
    }

    warn_about_risky_changes(entry, cmdline);

    if app.args.dry_run {
        dry_run_notice(&format!("set the command line of '{}' to:", entry.title));
        println!("  {cmdline}");
        return Ok(());
    }

    let outcomes = loader.set_cmdline(&app.context(), entry, cmdline)?;

    success(&format!("updated kernel parameters for {}", style::bold(&entry.title)));
    println!("  {} {}", style::dim("was:"), style::dim(&entry.cmdline));
    println!("  {} {cmdline}", style::dim("now:"));
    app.report_writes(&outcomes);
    app.print_pending(loader);
    app.print_note(loader);

    Ok(())
}

/// Point out changes that commonly render a system unbootable.
fn warn_about_risky_changes(entry: &BootEntry, new_cmdline: &str) {
    let old = split_cmdline(&entry.cmdline);
    let new = split_cmdline(new_cmdline);

    let had_root = old.iter().any(|p| param_key(p) == "root");
    let has_root = new.iter().any(|p| param_key(p) == "root");

    // Losing root= means the kernel boots and then panics with no filesystem.
    if had_root && !has_root {
        super::warn(
            "the new command line has no root= parameter; the kernel will panic \
             unless the root filesystem is found another way",
        );
    }

    let old_root = old.iter().find(|p| param_key(p) == "root");
    let new_root = new.iter().find(|p| param_key(p) == "root");
    if let (Some(a), Some(b)) = (old_root, new_root) {
        if a != b {
            super::warn(&format!("root filesystem changed from '{a}' to '{b}'"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_a_new_parameter() {
        let out = merge_params("root=UUID=abc ro", &["quiet".into()]);
        assert_eq!(out, "root=UUID=abc ro quiet");
    }

    #[test]
    fn replaces_rather_than_duplicating_a_key() {
        // The kernel takes the last occurrence, so a duplicate would work but
        // leave the config misleading to read.
        let out = merge_params("root=UUID=abc loglevel=3 ro", &["loglevel=7".into()]);
        assert_eq!(out, "root=UUID=abc loglevel=7 ro");
        assert_eq!(out.matches("loglevel").count(), 1);
    }

    #[test]
    fn keeps_parameter_order_when_replacing() {
        let out = merge_params("a=1 b=2 c=3", &["b=9".into()]);
        assert_eq!(out, "a=1 b=9 c=3");
    }

    #[test]
    fn accepts_several_parameters_in_one_argument() {
        let out = merge_params("ro", &["quiet splash".into()]);
        assert_eq!(out, "ro quiet splash");
    }

    #[test]
    fn removes_by_key_including_bare_flags() {
        assert_eq!(remove_params("root=UUID=abc ro quiet", &["quiet".into()]), "root=UUID=abc ro");
        assert_eq!(
            remove_params("root=UUID=abc ro quiet", &["root".into()]),
            "ro quiet",
            "a key removes its key=value parameter"
        );
    }

    #[test]
    fn removing_an_absent_key_changes_nothing() {
        assert_eq!(remove_params("ro quiet", &["splash".into()]), "ro quiet");
    }

    #[test]
    fn removes_several_keys_at_once() {
        assert_eq!(remove_params("a=1 b=2 c=3 d", &["b".into(), "d".into()]), "a=1 c=3");
    }

    #[test]
    fn handles_quoted_values_as_single_parameters() {
        let out = merge_params(r#"root=UUID=abc opt="a b""#, &["quiet".into()]);
        assert_eq!(out, r#"root=UUID=abc opt="a b" quiet"#);
        // The quoted value must not be split into two parameters.
        assert_eq!(split_cmdline(&out).len(), 3);
    }

    #[test]
    fn extracts_parameter_keys() {
        assert_eq!(param_key("root=UUID=abc"), "root");
        assert_eq!(param_key("quiet"), "quiet");
        assert_eq!(param_key("a="), "a");
    }

    #[test]
    fn merging_into_an_empty_command_line_works() {
        assert_eq!(merge_params("", &["root=/dev/sda1".into()]), "root=/dev/sda1");
    }
}
