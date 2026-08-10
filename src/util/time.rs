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
//! Minimal UTC date formatting.
//!
//! kernelctl only ever needs to render a `SystemTime` as a human-readable
//! stamp: build dates in the entry table and timestamps in backup filenames.
//! That is a few dozen lines of civil-calendar arithmetic, so we do it here
//! instead of taking on a full date/time crate.

use std::time::{SystemTime, UNIX_EPOCH};

/// Broken-down UTC time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Utc {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

/// Convert a count of days since 1970-01-01 into a civil date.
///
/// This is Howard Hinnant's `civil_from_days`, which shifts the epoch to
/// 0000-03-01 so that the leap day lands at the end of the year and the
/// month-length pattern becomes a simple linear formula.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], March-based
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

impl Utc {
    /// Break a Unix timestamp down into UTC calendar fields.
    pub fn from_unix(secs: i64) -> Utc {
        // Floor-divide so timestamps before the epoch still land on the right
        // day; a truncating division would round them toward zero.
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        Utc {
            year,
            month,
            day,
            hour: (rem / 3600) as u32,
            minute: (rem % 3600 / 60) as u32,
            second: (rem % 60) as u32,
        }
    }

    pub fn from_system_time(t: SystemTime) -> Utc {
        Utc::from_unix(unix_secs(t))
    }

    pub fn now() -> Utc {
        Utc::from_system_time(SystemTime::now())
    }

    /// `2026-08-08 14:03` - the entry table's build-date column.
    pub fn format_minutes(&self) -> String {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute
        )
    }

    /// `2026-08-08` - date only.
    pub fn format_date(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// `2026-08-08T14:03:52Z` - RFC 3339, for machine-readable output.
    ///
    /// Always UTC, so consumers never have to guess an offset.
    pub fn format_rfc3339(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// `20260808-140352` - safe for use inside a filename.
    pub fn format_stamp(&self) -> String {
        format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// Seconds since the Unix epoch, negative for times before it.
pub fn unix_secs(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// Render a `SystemTime` as `YYYY-MM-DD HH:MM` in UTC.
pub fn format_time(t: SystemTime) -> String {
    Utc::from_system_time(t).format_minutes()
}

/// A coarse "how long ago" label for the details panel.
pub fn relative_to_now(t: SystemTime) -> String {
    let now = unix_secs(SystemTime::now());
    let then = unix_secs(t);
    let delta = now - then;
    if delta < 0 {
        return "in the future".into();
    }
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const MONTH: i64 = 30 * DAY;
    const YEAR: i64 = 365 * DAY;

    let (n, unit) = match delta {
        d if d < MINUTE => return "just now".into(),
        d if d < HOUR => (d / MINUTE, "minute"),
        d if d < DAY => (d / HOUR, "hour"),
        d if d < MONTH => (d / DAY, "day"),
        d if d < YEAR => (d / MONTH, "month"),
        d => (d / YEAR, "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

/// Human-readable byte count with binary units, e.g. `1.4 GiB`.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    // One decimal below 10 keeps the column narrow without losing precision
    // where it matters (9.7 GiB reads better than 10 GiB).
    if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn converts_known_timestamps() {
        let t = Utc::from_unix(0);
        assert_eq!(t.format_minutes(), "1970-01-01 00:00");

        // 2026-08-08T14:03:52Z
        let t = Utc::from_unix(1_786_197_832);
        assert_eq!(t.format_minutes(), "2026-08-08 14:03");
        assert_eq!(t.format_stamp(), "20260808-140352");
    }

    #[test]
    fn handles_leap_days() {
        // 2024-02-29T00:00:00Z
        let t = Utc::from_unix(1_709_164_800);
        assert_eq!(t.format_date(), "2024-02-29");
        // 2000 was a leap year (divisible by 400); 1900 was not.
        let t = Utc::from_unix(951_782_400); // 2000-02-29
        assert_eq!(t.format_date(), "2000-02-29");
    }

    #[test]
    fn handles_pre_epoch_timestamps() {
        // -1 second is 1969-12-31 23:59:59, not 1970-01-01.
        let t = Utc::from_unix(-1);
        assert_eq!(t.format_minutes(), "1969-12-31 23:59");
    }

    #[test]
    fn formats_year_boundaries() {
        let t = Utc::from_unix(1_767_225_599); // 2025-12-31T23:59:59Z
        assert_eq!(t.format_date(), "2025-12-31");
        let t = Utc::from_unix(1_767_225_600); // 2026-01-01T00:00:00Z
        assert_eq!(t.format_date(), "2026-01-01");
    }

    #[test]
    fn formats_byte_sizes() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(1_536), "1.5 KiB");
        assert_eq!(format_bytes(15 * 1024 * 1024), "15 MiB");
        assert_eq!(format_bytes(1_610_612_736), "1.5 GiB");
    }

    #[test]
    fn describes_relative_times() {
        let now = SystemTime::now();
        assert_eq!(relative_to_now(now), "just now");
        assert_eq!(relative_to_now(now - Duration::from_secs(3600)), "1 hour ago");
        assert_eq!(relative_to_now(now - Duration::from_secs(3 * 86400)), "3 days ago");
    }
}
