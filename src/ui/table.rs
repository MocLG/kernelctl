//! Column-aligned table rendering for `list` and friends.
//!
//! Widths are computed from the actual content rather than fixed, and the
//! table shrinks to fit the terminal by trimming the most compressible column
//! first - the title, which is long and usually still recognisable truncated -
//! rather than wrapping, which would break one entry across several lines and
//! make the output hard to scan.

use super::style;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

#[derive(Debug, Clone)]
pub struct Column {
    pub header: String,
    pub align: Align,
    /// Columns with a higher flex value give up space first when the table is
    /// too wide. Zero means the column is never truncated.
    pub flex: u8,
    /// Never shrink below this many columns.
    pub min_width: usize,
}

impl Column {
    pub fn new(header: &str) -> Column {
        Column {
            header: header.to_string(),
            align: Align::Left,
            flex: 0,
            min_width: 3,
        }
    }

    pub fn right(mut self) -> Column {
        self.align = Align::Right;
        self
    }

    /// Mark this column as the one to shrink when space runs out.
    pub fn flexible(mut self, flex: u8, min_width: usize) -> Column {
        self.flex = flex;
        self.min_width = min_width;
        self
    }
}

pub struct Table {
    columns: Vec<Column>,
    rows: Vec<Vec<String>>,
    /// Gap between columns.
    gap: usize,
}

impl Table {
    pub fn new(columns: Vec<Column>) -> Table {
        Table { columns, rows: Vec::new(), gap: 2 }
    }

    pub fn push(&mut self, row: Vec<String>) {
        debug_assert_eq!(row.len(), self.columns.len(), "row must match column count");
        self.rows.push(row);
    }

    /// Natural width of each column: the widest cell, header included.
    fn natural_widths(&self) -> Vec<usize> {
        self.columns
            .iter()
            .enumerate()
            .map(|(i, col)| {
                let widest =
                    self.rows.iter().filter_map(|r| r.get(i)).map(|c| style::width(c)).max();
                style::width(&col.header).max(widest.unwrap_or(0))
            })
            .collect()
    }

    /// Fit the columns into `available` display columns.
    fn fitted_widths(&self, available: usize) -> Vec<usize> {
        let mut widths = self.natural_widths();
        let gaps = self.gap * self.columns.len().saturating_sub(1);
        let mut total: usize = widths.iter().sum::<usize>() + gaps;

        if total <= available {
            return widths;
        }

        // Take space from the most flexible columns first, down to their
        // minimum. Anything with flex 0 - ids, badges, versions - keeps its
        // full width, because a truncated id is useless.
        let mut order: Vec<usize> = (0..self.columns.len()).collect();
        order.sort_by_key(|i| std::cmp::Reverse(self.columns[*i].flex));

        for i in order {
            if total <= available || self.columns[i].flex == 0 {
                continue;
            }
            let excess = total - available;
            let shrinkable = widths[i].saturating_sub(self.columns[i].min_width);
            let take = shrinkable.min(excess);
            widths[i] -= take;
            total -= take;
        }

        widths
    }

    /// Pad a cell to `width`, accounting for any styling it carries.
    fn pad(cell: &str, width: usize, align: Align, last: bool) -> String {
        let cell = style::truncate_styled(cell, width);
        let w = style::width(&cell);
        let padding = width.saturating_sub(w);

        match align {
            // Trailing whitespace on the last column is pointless and shows up
            // in diffs when output is redirected.
            Align::Left if last => cell,
            Align::Left => format!("{cell}{}", " ".repeat(padding)),
            Align::Right => format!("{}{cell}", " ".repeat(padding)),
        }
    }

    /// Render the table, wrapping to `terminal_width` columns.
    pub fn render(&self, terminal_width: usize) -> String {
        let widths = self.fitted_widths(terminal_width);
        let gap = " ".repeat(self.gap);
        let mut out = String::new();

        let last = self.columns.len().saturating_sub(1);

        let header: Vec<String> = self
            .columns
            .iter()
            .enumerate()
            .map(|(i, c)| {
                Table::pad(&style::heading(&c.header.to_uppercase()), widths[i], c.align, i == last)
            })
            .collect();
        out.push_str(&header.join(&gap));
        out.push('\n');

        for row in &self.rows {
            let cells: Vec<String> = row
                .iter()
                .enumerate()
                .map(|(i, cell)| {
                    Table::pad(cell, widths[i], self.columns[i].align, i == last)
                })
                .collect();
            out.push_str(&cells.join(&gap));
            out.push('\n');
        }

        out
    }
}

/// Terminal width, defaulting to a sensible 100 when it cannot be determined
/// (a pipe, a CI log) so redirected output is not squeezed into 80 columns.
pub fn terminal_width() -> usize {
    if let Ok(size) = rustix::termios::tcgetwinsize(std::io::stdout()) {
        if size.ws_col > 0 {
            return size.ws_col as usize;
        }
    }
    if let Ok(cols) = std::env::var("COLUMNS") {
        if let Ok(n) = cols.parse::<usize>() {
            if n > 0 {
                return n;
            }
        }
    }
    100
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Table {
        let mut t = Table::new(vec![
            Column::new("id"),
            Column::new("title").flexible(2, 10),
            Column::new("version"),
            Column::new("size").right(),
        ]);
        t.push(vec![
            "grub2-a1b2c3".into(),
            "Ubuntu 24.04 LTS".into(),
            "6.11.0-9-generic".into(),
            "14 MiB".into(),
        ]);
        t.push(vec!["grub2-d4e5f6".into(), "Windows".into(), "-".into(), "-".into()]);
        t
    }

    #[test]
    fn renders_aligned_columns() {
        let out = sample().render(120);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "header plus two rows");
        assert!(lines[0].starts_with("ID"));

        // Every row's second column starts at the same offset.
        let offset = lines[1].find("Ubuntu").unwrap();
        assert_eq!(lines[2].find("Windows"), Some(offset));
    }

    #[test]
    fn right_aligns_where_asked() {
        let out = sample().render(120);
        // The size column is last, so its content ends the line.
        assert!(out.lines().nth(1).unwrap().ends_with("14 MiB"));
    }

    #[test]
    fn shrinks_the_flexible_column_first() {
        let narrow = sample().render(50);
        for line in narrow.lines() {
            assert!(style::width(line) <= 50, "line overflows: {line:?}");
        }
        // The id column has flex 0, so it survives intact.
        assert!(narrow.contains("grub2-a1b2c3"));
    }

    #[test]
    fn never_shrinks_below_the_minimum() {
        let out = sample().render(10);
        // It cannot fit, but the title keeps at least its declared minimum
        // rather than vanishing.
        assert!(out.lines().nth(1).unwrap().contains("Ubuntu"));
    }

    #[test]
    fn no_trailing_whitespace_on_the_last_column() {
        let mut t = Table::new(vec![Column::new("a"), Column::new("b")]);
        t.push(vec!["x".into(), "short".into()]);
        t.push(vec!["y".into(), "much longer value".into()]);
        for line in t.render(100).lines() {
            assert_eq!(line, line.trim_end(), "trailing whitespace in {line:?}");
        }
    }

    #[test]
    fn handles_an_empty_table() {
        let t = Table::new(vec![Column::new("id"), Column::new("title")]);
        // Just the header row.
        assert_eq!(t.render(80).lines().count(), 1);
    }

    #[test]
    fn aligns_rows_containing_styled_cells() {
        let mut t = Table::new(vec![Column::new("state"), Column::new("title")]);
        t.push(vec![style::badge("DEFAULT"), "Arch".into()]);
        t.push(vec!["".into(), "Fedora".into()]);

        let out = t.render(80);
        let lines: Vec<&str> = out.lines().collect();
        // Widths are measured on visible text, so both titles line up even
        // though one row carries escape codes.
        assert_eq!(
            style::strip_ansi(lines[1]).find("Arch"),
            style::strip_ansi(lines[2]).find("Fedora")
        );
    }

    #[test]
    fn terminal_width_is_always_usable() {
        assert!(terminal_width() >= 20);
    }
}
