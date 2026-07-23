//! `kernelctl status` - a one-screen summary of how this machine boots.

use serde::Serialize;

use crate::error::Result;
use crate::loaders::Timeout;
use crate::sys::mounts;
use crate::ui::style;
use crate::util::time;

use super::{print_json, App};

#[derive(Serialize)]
struct SpaceReport {
    mount: String,
    filesystem: String,
    total: u64,
    available: u64,
    used_percent: f64,
}

#[derive(Serialize)]
struct StatusReport {
    hostname: String,
    distro: Option<String>,
    kernel: String,
    architecture: String,
    firmware: String,
    privileges: &'static str,
    bootloader: Option<String>,
    bootloader_confidence: Option<u8>,
    other_bootloaders: Vec<String>,
    default_entry: Option<String>,
    oneshot_entry: Option<String>,
    timeout: Option<String>,
    entry_count: usize,
    broken_entries: usize,
    boot_space: Vec<SpaceReport>,
}

pub fn run(app: &App) -> Result<()> {
    let loader = app.loader().ok();

    // A failure to parse the config must not take the whole status report
    // down: the host facts and disk space are still worth showing, and the
    // parse error is itself the most useful thing to report.
    let (entries, entry_error) = match app.entries() {
        Ok(e) => (e, None),
        Err(e) => (Vec::new(), Some(e)),
    };

    let default = entries.iter().find(|e| e.is_default());
    let oneshot = entries.iter().find(|e| e.is_oneshot());
    let broken = entries.iter().filter(|e| e.flags.contains(crate::model::EntryFlags::BROKEN)).count();

    let timeout = loader.and_then(|l| l.timeout(&app.context()).ok().flatten());

    let space = collect_space(app);

    if app.args.json {
        let report = StatusReport {
            hostname: app.host.hostname.clone(),
            distro: app.host.distro.clone(),
            kernel: app.host.kernel_release.clone(),
            architecture: app.host.arch.to_string(),
            firmware: app.host.firmware().to_string(),
            privileges: if app.privileges.root { "root" } else { "user" },
            bootloader: loader.map(|l| l.kind().to_string()),
            bootloader_confidence: loader.map(|l| l.confidence()),
            other_bootloaders: app
                .discovery
                .kinds()
                .iter()
                .skip(1)
                .map(|k| k.to_string())
                .collect(),
            default_entry: default.map(|e| e.title.clone()),
            oneshot_entry: oneshot.map(|e| e.title.clone()),
            timeout: timeout.map(|t| t.to_string()),
            entry_count: entries.len(),
            broken_entries: broken,
            boot_space: space,
        };
        return print_json(&report);
    }

    let mut rows: Vec<(&str, String)> = Vec::new();

    rows.push(("host", format!("{} ({})", app.host.hostname, app.host.distro_label())));
    rows.push(("kernel", app.host.kernel_release.clone()));
    rows.push(("architecture", format!("{} ({})", app.host.arch, app.host.machine)));
    rows.push(("firmware", app.host.firmware().to_string()));
    rows.push((
        "privileges",
        if app.privileges.root {
            style::paint(style::Style::Green, "root (writes enabled)")
        } else {
            style::paint(style::Style::Yellow, "user (read-only; writes need sudo)")
        },
    ));

    match loader {
        Some(l) => {
            rows.push((
                "bootloader",
                format!("{} ({}% confidence)", l.kind().display_name(), l.confidence()),
            ));
            let others: Vec<&str> =
                app.discovery.kinds().iter().skip(1).map(|k| k.display_name()).collect();
            if !others.is_empty() {
                rows.push(("also present", style::dim(&others.join(", "))));
            }
        }
        None => rows.push((
            "bootloader",
            style::paint(style::Style::BoldRed, "none detected"),
        )),
    }

    match default {
        Some(e) => rows.push(("default entry", format!("{} ({})", e.title, e.id))),
        None if loader.is_some() => {
            rows.push(("default entry", style::dim("not set; the loader picks its own")))
        }
        None => {}
    }
    if let Some(e) = oneshot {
        rows.push((
            "next boot",
            style::paint(style::Style::BoldYellow, &format!("{} (one-shot)", e.title)),
        ));
    }
    if let Some(t) = timeout {
        rows.push(("menu timeout", format_timeout(t)));
    }

    if entries.is_empty() {
        if let Some(err) = &entry_error {
            rows.push(("entries", style::paint(style::Style::Red, &format!("unreadable: {err}"))));
        }
    } else {
        let summary = if broken > 0 {
            format!(
                "{} ({})",
                entries.len(),
                style::paint(style::Style::BoldRed, &format!("{broken} broken"))
            )
        } else {
            entries.len().to_string()
        };
        rows.push(("boot entries", summary));
    }

    let width = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    println!("{}", style::heading("System"));
    for (key, value) in &rows {
        println!("  {}{:pad$}  {value}", style::label(key), "", pad = width - key.len());
    }

    print_space(app);

    if let Some(err) = entry_error {
        println!();
        super::warn(&format!("boot entries could not be read: {err}"));
        if let Some(hint) = err.hint() {
            super::note_line(&hint);
        }
    }

    Ok(())
}

fn format_timeout(t: Timeout) -> String {
    match t {
        Timeout::Immediate => style::dim("0s (menu hidden)"),
        Timeout::Seconds(n) => format!("{n}s"),
        Timeout::Indefinite => "no timeout (waits for input)".into(),
    }
}

fn collect_space(app: &App) -> Vec<SpaceReport> {
    mounts::boot_mounts(&app.roots.mounts)
        .into_iter()
        .filter_map(|m| {
            let info = mounts::space_for(&m.target).ok()?;
            Some(SpaceReport {
                mount: m.target.display().to_string(),
                filesystem: m.fstype.clone(),
                total: info.total,
                available: info.available,
                used_percent: info.used_percent(),
            })
        })
        .collect()
}

fn print_space(app: &App) {
    let mounts = mounts::boot_mounts(&app.roots.mounts);
    if mounts.is_empty() {
        return;
    }

    println!();
    println!("{}", style::heading("Boot storage"));

    let mut table = crate::ui::table::Table::new(vec![
        crate::ui::table::Column::new("mount"),
        crate::ui::table::Column::new("type"),
        crate::ui::table::Column::new("size").right(),
        crate::ui::table::Column::new("free").right(),
        crate::ui::table::Column::new("used").right(),
    ]);

    for m in mounts {
        let Ok(info) = mounts::space_for(&m.target) else { continue };
        // A boot partition filling up is how a kernel upgrade fails, so a
        // nearly-full one is called out rather than left to be noticed.
        let used = format!("{:.0}%", info.used_percent());
        let used = if info.is_low() {
            style::paint(style::Style::BoldRed, &used)
        } else {
            used
        };

        table.push(vec![
            m.target.display().to_string(),
            m.fstype.clone(),
            time::format_bytes(info.total),
            time::format_bytes(info.available),
            used,
        ]);
    }

    print!("{}", table.render(crate::ui::table::terminal_width()));

    if mounts::boot_mounts(&app.roots.mounts)
        .iter()
        .any(|m| mounts::space_for(&m.target).map(|s| s.is_low()).unwrap_or(false))
    {
        super::note_line("a boot filesystem is nearly full; `kernelctl clean` can reclaim space");
    }
}
