//! `kernelctl diff` - compare two boot entries.
//!
//! The usual reason to run this is "the old kernel boots and the new one does
//! not, what is different?", so the output leads with the kernel parameters
//! and shows added and removed ones separately rather than as two blobs of
//! text to eyeball.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::error::Result;
use crate::model::{split_cmdline, BootEntry};
use crate::ui::style;

use super::{print_json, App};

#[derive(Serialize)]
struct FieldDiff {
    field: String,
    first: String,
    second: String,
}

#[derive(Serialize)]
struct DiffReport {
    first: String,
    second: String,
    fields: Vec<FieldDiff>,
    cmdline_only_in_first: Vec<String>,
    cmdline_only_in_second: Vec<String>,
    cmdline_changed: Vec<FieldDiff>,
    identical: bool,
}

pub fn run(app: &App, first: &str, second: &str) -> Result<()> {
    let entries = app.entries()?;
    let a = crate::loaders::registry::resolve(&entries, first)?;
    let b = crate::loaders::registry::resolve(&entries, second)?;

    let fields = compare_fields(a, b);
    let (only_a, only_b, changed) = compare_cmdlines(&a.cmdline, &b.cmdline);
    let identical = fields.is_empty() && only_a.is_empty() && only_b.is_empty() && changed.is_empty();

    if app.args.json {
        return print_json(&DiffReport {
            first: a.id.clone(),
            second: b.id.clone(),
            fields,
            cmdline_only_in_first: only_a,
            cmdline_only_in_second: only_b,
            cmdline_changed: changed,
            identical,
        });
    }

    println!(
        "{} {}\n{} {}\n",
        style::paint(style::Style::BoldRed, "-"),
        style::bold(&format!("{} ({})", a.title, a.id)),
        style::paint(style::Style::BoldGreen, "+"),
        style::bold(&format!("{} ({})", b.title, b.id)),
    );

    if identical {
        println!("the two entries are identical in every compared field");
        return Ok(());
    }

    if !only_a.is_empty() || !only_b.is_empty() || !changed.is_empty() {
        println!("{}", style::heading("Kernel parameters"));
        for param in &only_a {
            println!("  {}", style::paint(style::Style::Red, &format!("- {param}")));
        }
        for param in &only_b {
            println!("  {}", style::paint(style::Style::Green, &format!("+ {param}")));
        }
        for c in &changed {
            println!(
                "  {} {} {} {}",
                style::paint(style::Style::Yellow, "~"),
                c.field,
                style::paint(style::Style::Red, &c.first),
                style::paint(style::Style::Green, &format!("-> {}", c.second)),
            );
        }
        println!();
    }

    if !fields.is_empty() {
        println!("{}", style::heading("Other differences"));
        let width = fields.iter().map(|f| f.field.len()).max().unwrap_or(0);
        for f in &fields {
            println!(
                "  {}{:pad$}  {}\n  {}{:pad$}  {}",
                style::label(&f.field),
                "",
                style::paint(style::Style::Red, &f.first),
                " ".repeat(f.field.len()),
                "",
                style::paint(style::Style::Green, &f.second),
                pad = width - f.field.len(),
            );
        }
    }

    Ok(())
}

/// Compare everything except the command line, which is handled separately.
fn compare_fields(a: &BootEntry, b: &BootEntry) -> Vec<FieldDiff> {
    let mut out = Vec::new();

    let mut push = |field: &str, x: String, y: String| {
        if x != y {
            out.push(FieldDiff { field: field.to_string(), first: x, second: y });
        }
    };

    push("title", a.title.clone(), b.title.clone());
    push("loader", a.loader.to_string(), b.loader.to_string());
    push("arch", a.arch.to_string(), b.arch.to_string());
    push(
        "version",
        a.version.as_ref().map(|v| v.raw.clone()).unwrap_or_else(|| "-".into()),
        b.version.as_ref().map(|v| v.raw.clone()).unwrap_or_else(|| "-".into()),
    );
    push("kernel", path_str(a.kernel.as_ref()), path_str(b.kernel.as_ref()));
    push(
        "initrd",
        a.initrds.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "),
        b.initrds.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "),
    );
    push("devicetree", path_str(a.devicetree.as_ref()), path_str(b.devicetree.as_ref()));
    push(
        "state",
        super::entries::badges_plain(a),
        super::entries::badges_plain(b),
    );
    push("source", a.source.display().to_string(), b.source.display().to_string());

    out
}

fn path_str(p: Option<&std::path::PathBuf>) -> String {
    p.map(|p| p.display().to_string()).unwrap_or_else(|| "-".into())
}

/// Split two command lines into removed, added and changed parameters.
///
/// A `key=value` parameter present in both with different values is reported
/// as one changed line rather than an unrelated removal and addition, which is
/// what makes the output readable.
fn compare_cmdlines(a: &str, b: &str) -> (Vec<String>, Vec<String>, Vec<FieldDiff>) {
    let a_params = split_cmdline(a);
    let b_params = split_cmdline(b);

    let key = |p: &str| p.split_once('=').map(|(k, _)| k.to_string()).unwrap_or_else(|| p.to_string());

    let a_set: BTreeSet<&String> = a_params.iter().collect();
    let b_set: BTreeSet<&String> = b_params.iter().collect();

    let mut only_a: Vec<String> = Vec::new();
    let mut only_b: Vec<String> = Vec::new();
    let mut changed: Vec<FieldDiff> = Vec::new();

    for param in &a_params {
        if b_set.contains(param) {
            continue;
        }
        let k = key(param);
        // Same key on both sides means the value changed.
        match b_params.iter().find(|p| key(p) == k && !a_set.contains(p)) {
            Some(other) => {
                if !changed.iter().any(|c| c.field == k) {
                    changed.push(FieldDiff {
                        field: k,
                        first: param.clone(),
                        second: other.clone(),
                    });
                }
            }
            None => only_a.push(param.clone()),
        }
    }

    for param in &b_params {
        if a_set.contains(param) {
            continue;
        }
        let k = key(param);
        if changed.iter().any(|c| c.field == k) {
            continue;
        }
        only_b.push(param.clone());
    }

    (only_a, only_b, changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LoaderKind;
    use std::path::PathBuf;

    #[test]
    fn reports_added_and_removed_parameters() {
        let (only_a, only_b, changed) =
            compare_cmdlines("root=UUID=abc ro quiet", "root=UUID=abc ro splash");
        assert_eq!(only_a, vec!["quiet"]);
        assert_eq!(only_b, vec!["splash"]);
        assert!(changed.is_empty());
    }

    #[test]
    fn reports_a_changed_value_as_one_line() {
        let (only_a, only_b, changed) = compare_cmdlines("loglevel=3 ro", "loglevel=7 ro");
        // Not an unrelated removal plus addition: it is one parameter that
        // changed, and saying so is what makes the output readable.
        assert!(only_a.is_empty());
        assert!(only_b.is_empty());
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].field, "loglevel");
        assert_eq!(changed[0].first, "loglevel=3");
        assert_eq!(changed[0].second, "loglevel=7");
    }

    #[test]
    fn identical_command_lines_produce_nothing() {
        let (a, b, c) = compare_cmdlines("root=UUID=abc ro quiet", "root=UUID=abc ro quiet");
        assert!(a.is_empty() && b.is_empty() && c.is_empty());
    }

    #[test]
    fn parameter_order_does_not_count_as_a_difference() {
        let (a, b, c) = compare_cmdlines("ro quiet splash", "splash ro quiet");
        assert!(a.is_empty() && b.is_empty() && c.is_empty());
    }

    #[test]
    fn handles_an_empty_command_line_on_one_side() {
        let (only_a, only_b, _) = compare_cmdlines("ro quiet", "");
        assert_eq!(only_a.len(), 2);
        assert!(only_b.is_empty());
    }

    #[test]
    fn compares_entry_fields() {
        let mut a = BootEntry::new(LoaderKind::Grub2, "/boot/grub/grub.cfg", "a", "Linux 6.11");
        a.kernel = Some(PathBuf::from("/boot/vmlinuz-6.11"));
        let mut b = BootEntry::new(LoaderKind::Grub2, "/boot/grub/grub.cfg", "b", "Linux 6.10");
        b.kernel = Some(PathBuf::from("/boot/vmlinuz-6.10"));

        let fields = compare_fields(&a, &b);
        let names: Vec<&str> = fields.iter().map(|f| f.field.as_str()).collect();
        assert!(names.contains(&"title"));
        assert!(names.contains(&"kernel"));
        // Identical fields are omitted rather than listed as unchanged.
        assert!(!names.contains(&"loader"));
    }

    #[test]
    fn identical_entries_differ_in_nothing() {
        let a = BootEntry::new(LoaderKind::Limine, "/boot/limine.conf", "x", "Arch");
        let b = a.clone();
        assert!(compare_fields(&a, &b).is_empty());
    }
}
