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
//! Drawing the interactive interface.

use ratatui::prelude::*;
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Table, TableState, Wrap,
};

use crate::commands::entries as entry_fmt;
use crate::commands::help::KEYBINDINGS;
use crate::model::{BootEntry, EntryFlags};
use crate::util::time;

use super::state::{Level, Modal, Tui};

/// Colour for a state badge, matching the CLI's palette so the two look like
/// one program.
fn badge_style(label: &str) -> Style {
    match label {
        "DEFAULT" => Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
        "ONESHOT" => Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        "RUNNING" => Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        "BROKEN" | "FOREIGN" => Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        "RECOVERY" => Style::new().fg(Color::Yellow),
        "DISABLED" => Style::new().fg(Color::DarkGray),
        "EFI-STUB" | "UKI" => Style::new().fg(Color::Magenta),
        _ => Style::new().fg(Color::DarkGray),
    }
}

fn badges_spans(entry: &BootEntry) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for label in entry.flags.badges() {
        if !spans.is_empty() {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(format!("[{label}]"), badge_style(label)));
    }
    spans
}

pub fn draw(frame: &mut Frame, tui: &mut Tui) {
    let area = frame.area();

    let chunks = Layout::vertical([
        Constraint::Length(3), // header
        Constraint::Min(6),    // body
        Constraint::Length(2), // footer
    ])
    .split(area);

    draw_header(frame, chunks[0], tui);
    draw_body(frame, chunks[1], tui);
    draw_footer(frame, chunks[2], tui);

    match &tui.modal {
        Modal::None => {}
        Modal::Help => draw_help(frame, area),
        Modal::Filter(input) => {
            draw_prompt(frame, area, "Filter entries", input.value(), input.cursor_column(), "type to filter, Enter to keep, Esc to clear")
        }
        Modal::EditCmdline { title, input, .. } => draw_prompt(
            frame,
            area,
            &format!("Kernel parameters - {title}"),
            input.value(),
            input.cursor_column(),
            "Enter to save, Esc to cancel, ctrl-w delete word, ctrl-u clear",
        ),
        Modal::Timeout(input) => draw_prompt(
            frame,
            area,
            "Boot menu timeout",
            input.value(),
            input.cursor_column(),
            "seconds, 0 for no menu, or 'never' to wait for input",
        ),
        Modal::Clean { candidates, scroll } => draw_clean(frame, area, candidates, *scroll),
        Modal::Confirm { prompt, .. } => draw_confirm(frame, area, prompt),
    }
}

fn draw_header(frame: &mut Frame, area: Rect, tui: &Tui) {
    let host = &tui.app.host;
    let loader = tui
        .app
        .loader()
        .map(|l| l.kind().display_name().to_string())
        .unwrap_or_else(|_| "no bootloader".to_string());

    // Reuse the CLI's badge text so the two interfaces cannot disagree.
    let privilege = tui.app.privileges.badge();
    let privilege_style = if tui.app.privileges.root {
        Style::new().fg(Color::Green).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    };

    let label = Style::new().fg(Color::DarkGray);
    let value = Style::new().fg(Color::White).add_modifier(Modifier::BOLD);

    let line = Line::from(vec![
        Span::styled("kernelctl", Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled("host ", label),
        Span::styled(host.hostname.clone(), value),
        Span::raw("  "),
        Span::styled("kernel ", label),
        Span::styled(host.kernel_release.clone(), value),
        Span::raw("  "),
        Span::styled("arch ", label),
        Span::styled(host.arch.to_string(), value),
        Span::raw("  "),
        Span::styled("loader ", label),
        Span::styled(loader, value),
        Span::raw("  "),
        Span::styled(privilege, privilege_style),
    ]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::DarkGray));

    frame.render_widget(Paragraph::new(line).block(block), area);
}

fn draw_body(frame: &mut Frame, area: Rect, tui: &mut Tui) {
    // Below this width a side-by-side split leaves both panes unreadable, so
    // stack them instead.
    let panes = if area.width >= 100 {
        Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).split(area)
    } else {
        Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).split(area)
    };

    draw_table(frame, panes[0], tui);
    draw_details(frame, panes[1], tui);
}

fn draw_table(frame: &mut Frame, area: Rect, tui: &mut Tui) {
    let title = if tui.filter.is_empty() {
        format!(" Boot entries ({}) ", tui.visible.len())
    } else {
        format!(" Boot entries ({}/{}) filter: {} ", tui.visible.len(), tui.entries.len(), tui.filter)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::DarkGray))
        .title(Span::styled(title, Style::new().fg(Color::Cyan)));

    if tui.visible.is_empty() {
        let message = if tui.entries.is_empty() {
            "No boot entries found.\n\nPress r to re-read, or start kernelctl with --boot-dir."
        } else {
            "No entries match the filter.\n\nPress Esc to clear it."
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(Style::new().fg(Color::DarkGray))
                .block(block)
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    let rows: Vec<Row> = tui
        .visible
        .iter()
        .map(|i| {
            let entry = &tui.entries[*i];
            let title_style = if entry.flags.contains(EntryFlags::BROKEN) {
                Style::new().fg(Color::Red)
            } else if entry.is_default() {
                Style::new().fg(Color::Green).add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            };

            Row::new(vec![
                Cell::from(Span::styled(entry_fmt::indented_title(entry), title_style)),
                Cell::from(entry_fmt::version(entry)),
                Cell::from(Span::styled(
                    entry_fmt::build_date(entry),
                    Style::new().fg(Color::DarkGray),
                )),
                Cell::from(Line::from(badges_spans(entry))),
            ])
        })
        .collect();

    let widths = [
        Constraint::Min(16),
        Constraint::Length(18),
        Constraint::Length(10),
        Constraint::Length(22),
    ];

    let table = Table::new(rows, widths)
        .header(
            Row::new(vec!["TITLE", "VERSION", "BUILT", "STATE"])
                .style(Style::new().fg(Color::White).add_modifier(Modifier::BOLD)),
        )
        .block(block)
        .row_highlight_style(
            Style::new().bg(Color::Indexed(236)).add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    let mut state = TableState::default().with_selected(Some(tui.cursor));
    frame.render_stateful_widget(table, area, &mut state);
    // Keep our own scroll offset in step with the widget's, so the scrollbar
    // and any future paging agree with what is on screen.
    tui.scroll = state.offset();

    if tui.visible.len() > area.height.saturating_sub(3) as usize {
        let mut scrollbar_state =
            ScrollbarState::new(tui.visible.len()).position(tui.cursor);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .style(Style::new().fg(Color::DarkGray)),
            area.inner(Margin { vertical: 1, horizontal: 0 }),
            &mut scrollbar_state,
        );
    }
}

fn draw_details(frame: &mut Frame, area: Rect, tui: &Tui) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::DarkGray))
        .title(Span::styled(" Details ", Style::new().fg(Color::Cyan)));

    let Some(entry) = tui.selected() else {
        frame.render_widget(
            Paragraph::new("Nothing selected.")
                .style(Style::new().fg(Color::DarkGray))
                .block(block),
            area,
        );
        return;
    };

    let label = Style::new().fg(Color::Cyan);
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(Span::styled(
        entry.title.clone(),
        Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
    )));
    let badges = badges_spans(entry);
    if !badges.is_empty() {
        lines.push(Line::from(badges));
    }
    lines.push(Line::raw(""));

    let mut field = |name: &str, value: String| {
        lines.push(Line::from(vec![
            Span::styled(format!("{name:<11}"), label),
            Span::raw(value),
        ]));
    };

    field("id", entry.id.clone());
    field("loader", entry.loader.display_name().to_string());
    field("arch", entry.arch.to_string());
    if let Some(v) = &entry.version {
        field("version", v.raw.clone());
    }
    field(
        "kernel",
        entry.kernel.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "-".into()),
    );

    if entry.initrds.is_empty() {
        field("initrd", "-".into());
    } else {
        for (i, initrd) in entry.initrds.iter().enumerate() {
            // Load order matters, so each image gets its own line.
            field(if i == 0 { "initrd" } else { "" }, initrd.display().to_string());
        }
    }
    if let Some(dtb) = &entry.devicetree {
        field("devicetree", dtb.display().to_string());
    }
    if let Some(size) = entry.kernel_size {
        field("size", time::format_bytes(size));
    }
    if let Some(t) = entry.build_time {
        field("built", format!("{} ({})", time::format_time(t), time::relative_to_now(t)));
    }
    field("source", entry.source.display().to_string());
    for (k, v) in &entry.extra {
        field(k, v.clone());
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled("cmdline", label)));
    lines.push(Line::from(Span::raw(if entry.cmdline.is_empty() {
        "-".to_string()
    } else {
        entry.cmdline.clone()
    })));

    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_footer(frame: &mut Frame, area: Rect, tui: &Tui) {
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);

    // Transient feedback from the last action.
    if let Some(message) = &tui.message {
        let style = match message.level {
            Level::Info => Style::new().fg(Color::Cyan),
            Level::Success => Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
            Level::Warning => Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            Level::Error => Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        };
        let prefix = match message.level {
            Level::Info => "info: ",
            Level::Success => "ok: ",
            Level::Warning => "warning: ",
            Level::Error => "error: ",
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(prefix, style),
                Span::raw(message.text.clone()),
            ]))
            .wrap(Wrap { trim: true }),
            rows[0],
        );
    }

    let key = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let text = Style::new().fg(Color::DarkGray);

    let mut spans: Vec<Span> = Vec::new();
    for (k, description) in [
        ("d", "default"),
        ("n", "next"),
        ("e", "cmdline"),
        ("t", "timeout"),
        ("c", "clean"),
        ("b", "backup"),
        ("/", "filter"),
        ("q", "quit"),
    ] {
        if !spans.is_empty() {
            spans.push(Span::styled("  ", text));
        }
        spans.push(Span::styled(k, key));
        spans.push(Span::styled(format!(" {description}"), text));
    }
    // The help hint sits at the end of the line, where the eye lands last.
    spans.push(Span::styled("   ", text));
    spans.push(Span::styled("?", key));
    spans.push(Span::styled(" help", text));

    frame.render_widget(Paragraph::new(Line::from(spans)), rows[1]);
}

/// A centred box `percent_x` by `percent_y` of the screen.
fn centered(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);

    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1])[1]
}

fn modal_block(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Cyan))
        .title(Span::styled(
            format!(" {title} "),
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ))
}

fn draw_help(frame: &mut Frame, area: Rect) {
    let area = centered(70, 80, area);
    frame.render_widget(Clear, area);

    let key = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            "Keys",
            Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
    ];

    for (k, description) in KEYBINDINGS {
        lines.push(Line::from(vec![
            Span::styled(format!("  {k:<14}"), key),
            Span::raw(*description),
        ]));
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "Notes",
        Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
    )));
    for note in [
        "Every config write keeps the previous version alongside it as a .bak file.",
        "Before changing what boots, the kernel and initramfs it names are checked",
        "  to exist on disk - so a typo cannot leave the machine unbootable.",
        "Writes need root; without it kernelctl still reads everything.",
        "Each command here is also available from the shell: run `kernelctl help`.",
    ] {
        lines.push(Line::from(Span::styled(
            format!("  {note}"),
            Style::new().fg(Color::DarkGray),
        )));
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  press any key to close",
        Style::new().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
    )));

    frame.render_widget(
        Paragraph::new(lines).block(modal_block("Help")).wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_prompt(frame: &mut Frame, area: Rect, title: &str, value: &str, cursor: usize, hint: &str) {
    let area = centered(80, 24, area);
    frame.render_widget(Clear, area);

    let lines = vec![
        Line::raw(""),
        Line::from(Span::raw(value.to_string())),
        Line::raw(""),
        Line::from(Span::styled(hint, Style::new().fg(Color::DarkGray))),
    ];

    frame.render_widget(
        Paragraph::new(lines).block(modal_block(title)).wrap(Wrap { trim: false }),
        area,
    );

    // Place the terminal cursor so the user can see where typing will land.
    // The input box starts one row down and one column in from the border.
    let inner_width = area.width.saturating_sub(2) as usize;
    if inner_width > 0 {
        let row = cursor / inner_width;
        let column = cursor % inner_width;
        frame.set_cursor_position((
            area.x + 1 + column as u16,
            area.y + 2 + row as u16,
        ));
    }
}

fn draw_clean(frame: &mut Frame, area: Rect, candidates: &[crate::commands::clean::Candidate], scroll: usize) {
    let area = centered(80, 70, area);
    frame.render_widget(Clear, area);

    let total: u64 = candidates.iter().map(|c| c.size).sum();
    let files: usize = candidates.iter().map(|c| c.paths.len()).sum();

    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::raw("These files are not referenced by any boot entry, are not the "),
            Span::styled("running", Style::new().fg(Color::Cyan)),
            Span::raw(" kernel,"),
        ]),
        Line::raw("and are not the newest installed kernel."),
        Line::raw(""),
    ];

    for candidate in candidates {
        lines.push(Line::from(vec![
            Span::styled(
                candidate.version.clone(),
                Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  ({})", time::format_bytes(candidate.size)),
                Style::new().fg(Color::DarkGray),
            ),
        ]));
        for path in &candidate.paths {
            lines.push(Line::from(Span::styled(
                format!("    {}", path.display()),
                Style::new().fg(Color::DarkGray),
            )));
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::raw(format!("{files} files, ")),
        Span::styled(
            time::format_bytes(total),
            Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" would be freed."),
    ]));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "Enter to remove them, ↑/↓ to scroll, Esc to cancel",
        Style::new().fg(Color::DarkGray),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .block(modal_block("Clean up unused kernels"))
            .scroll((scroll as u16, 0))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_confirm(frame: &mut Frame, area: Rect, prompt: &str) {
    let area = centered(60, 22, area);
    frame.render_widget(Clear, area);

    let lines = vec![
        Line::raw(""),
        Line::from(Span::raw(prompt.to_string())),
        Line::raw(""),
        Line::from(vec![
            Span::styled("y", Style::new().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::styled(" confirm    ", Style::new().fg(Color::DarkGray)),
            Span::styled("n / Esc", Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)),
            Span::styled(" cancel", Style::new().fg(Color::DarkGray)),
        ]),
    ];

    frame.render_widget(
        Paragraph::new(lines).block(modal_block("Confirm")).wrap(Wrap { trim: true }),
        area,
    );
}
