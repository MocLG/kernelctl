//! Shared rendering of boot entries.
//!
//! `list`, `diff` and the TUI details panel all present the same facts, so the
//! formatting lives here and each caller only decides which parts to show.

use crate::model::BootEntry;
use crate::ui::style;
use crate::util::time;

/// Badges for an entry, styled and joined.
pub fn badges(entry: &BootEntry) -> String {
    entry.flags.badges().iter().map(|b| style::badge(b)).collect::<Vec<_>>().join(" ")
}

/// Plain-text badges, for JSON and for width calculations.
pub fn badges_plain(entry: &BootEntry) -> String {
    entry.flags.badges().iter().map(|b| format!("[{b}]")).collect::<Vec<_>>().join(" ")
}

/// Kernel version, or a placeholder when the entry has none.
pub fn version(entry: &BootEntry) -> String {
    entry.version.as_ref().map(|v| v.raw.clone()).unwrap_or_else(|| "-".into())
}

/// Build date, taken from the kernel image's mtime.
pub fn build_date(entry: &BootEntry) -> String {
    entry
        .build_time
        .map(|t| time::Utc::from_system_time(t).format_date())
        .unwrap_or_else(|| "-".into())
}

/// Kernel image size.
pub fn size(entry: &BootEntry) -> String {
    entry.kernel_size.map(time::format_bytes).unwrap_or_else(|| "-".into())
}

/// Title, indented to show submenu nesting.
pub fn indented_title(entry: &BootEntry) -> String {
    if entry.depth == 0 {
        entry.title.clone()
    } else {
        format!("{}{}", "  ".repeat(entry.depth as usize), entry.title)
    }
}

/// A path, or a dash when the entry has none.
fn path_or_dash(path: Option<&std::path::PathBuf>) -> String {
    path.map(|p| p.display().to_string()).unwrap_or_else(|| "-".into())
}

/// The multi-line detail view of one entry.
///
/// Shared by `list --long`, `diff` and the TUI preview panel so that all three
/// describe an entry the same way.
pub fn details(entry: &BootEntry) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = Vec::new();

    rows.push(("id".into(), entry.id.clone()));
    rows.push(("title".into(), entry.title.clone()));
    rows.push(("loader".into(), entry.loader.display_name().to_string()));

    if let Some(v) = &entry.version {
        rows.push(("version".into(), v.raw.clone()));
    }
    rows.push(("arch".into(), entry.arch.to_string()));

    let badges = badges_plain(entry);
    if !badges.is_empty() {
        rows.push(("state".into(), badges));
    }

    rows.push(("kernel".into(), path_or_dash(entry.kernel.as_ref())));

    if entry.initrds.is_empty() {
        rows.push(("initrd".into(), "-".into()));
    } else {
        // Each initrd on its own row: they are long paths and load order
        // matters, so they must not be collapsed onto one line.
        for (i, initrd) in entry.initrds.iter().enumerate() {
            let label = if i == 0 { "initrd".to_string() } else { String::new() };
            rows.push((label, initrd.display().to_string()));
        }
    }

    if let Some(dtb) = &entry.devicetree {
        rows.push(("devicetree".into(), dtb.display().to_string()));
    }

    rows.push((
        "cmdline".into(),
        if entry.cmdline.is_empty() { "-".into() } else { entry.cmdline.clone() },
    ));

    if entry.kernel_size.is_some() {
        rows.push(("size".into(), size(entry)));
    }
    if let Some(t) = entry.build_time {
        rows.push((
            "built".into(),
            format!("{} ({})", time::format_time(t), time::relative_to_now(t)),
        ));
    }

    rows.push(("source".into(), entry.source.display().to_string()));
    for (k, v) in &entry.extra {
        rows.push((k.clone(), v.clone()));
    }

    rows
}

/// Render detail rows as aligned `label: value` lines.
pub fn render_details(entry: &BootEntry, indent: &str) -> String {
    let rows = details(entry);
    let width = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);

    rows.iter()
        .map(|(k, v)| {
            if k.is_empty() {
                // Continuation row, e.g. a second initrd.
                format!("{indent}{:width$}  {v}", "")
            } else {
                format!("{indent}{}{:pad$}  {v}", style::label(k), "", pad = width - k.len())
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EntryFlags, KernelVersion, LoaderKind};
    use std::path::PathBuf;

    fn entry() -> BootEntry {
        let mut e =
            BootEntry::new(LoaderKind::SystemdBoot, "/boot/loader/entries/arch.conf", "arch.conf", "Arch Linux");
        e.version = KernelVersion::parse("6.11.5-arch1-1");
        e.kernel = Some(PathBuf::from("/boot/vmlinuz-linux"));
        e.initrds = vec![
            PathBuf::from("/boot/amd-ucode.img"),
            PathBuf::from("/boot/initramfs-linux.img"),
        ];
        e.cmdline = "root=UUID=abc rw quiet".into();
        e.flags.insert(EntryFlags::DEFAULT);
        e
    }

    #[test]
    fn describes_every_important_field() {
        let rows = details(&entry());
        let keys: Vec<&str> = rows.iter().map(|(k, _)| k.as_str()).collect();
        for expected in ["id", "title", "kernel", "cmdline", "source", "version", "state"] {
            assert!(keys.contains(&expected), "missing {expected} in {keys:?}");
        }
    }

    #[test]
    fn lists_each_initrd_on_its_own_row() {
        let rows = details(&entry());
        let initrd_rows: Vec<&(String, String)> =
            rows.iter().skip_while(|(k, _)| k != "initrd").take(2).collect();
        // Load order matters - microcode must stay first - so they are not
        // collapsed onto one line.
        assert!(initrd_rows[0].1.ends_with("amd-ucode.img"));
        assert!(initrd_rows[1].0.is_empty(), "continuation rows have no label");
        assert!(initrd_rows[1].1.ends_with("initramfs-linux.img"));
    }

    #[test]
    fn shows_placeholders_for_absent_fields() {
        let bare = BootEntry::new(LoaderKind::Lilo, "/etc/lilo.conf", "win", "Windows");
        let rows = details(&bare);
        let map: std::collections::HashMap<_, _> =
            rows.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        assert_eq!(map.get("kernel"), Some(&"-"));
        assert_eq!(map.get("initrd"), Some(&"-"));
        assert_eq!(map.get("cmdline"), Some(&"-"));
    }

    #[test]
    fn indents_nested_titles() {
        let mut e = entry();
        assert_eq!(indented_title(&e), "Arch Linux");
        e.depth = 1;
        assert_eq!(indented_title(&e), "  Arch Linux");
    }

    #[test]
    fn renders_aligned_label_columns() {
        let out = render_details(&entry(), "  ");
        for line in out.lines() {
            assert!(line.starts_with("  "), "indent applied to {line:?}");
        }
        assert!(style::strip_ansi(&out).contains("cmdline"));
    }

    #[test]
    fn version_and_size_fall_back_to_a_dash() {
        let bare = BootEntry::new(LoaderKind::Uki, "/boot/x.efi", "x.efi", "x");
        assert_eq!(version(&bare), "-");
        assert_eq!(size(&bare), "-");
        assert_eq!(build_date(&bare), "-");
    }
}
