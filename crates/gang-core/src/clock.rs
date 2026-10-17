//! Timestamps as RFC 3339 text, without a date library.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `2026-10-17T14:32:10Z`.
pub fn format(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Reads `2026-10-17T14:32:10Z` or `2026-10-17T14:32:10+03:00` (fractions of
/// a second are ignored).
pub fn parse(text: &str) -> Option<i64> {
    let text = text.trim();
    let (date, time) = text.split_once(['T', ' '])?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;

    let (clock, offset) = if let Some(clock) = time.strip_suffix('Z') {
        (clock, 0)
    } else {
        let pos = time.rfind(['+', '-'])?;
        let (clock, zone) = time.split_at(pos);
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        let (hours, minutes) = zone[1..].split_once(':')?;
        let offset = sign * (hours.parse::<i64>().ok()? * 3600 + minutes.parse::<i64>().ok()? * 60);
        (clock, offset)
    };
    let mut clock_parts = clock.split(':');
    let hour: i64 = clock_parts.next()?.parse().ok()?;
    let minute: i64 = clock_parts.next()?.parse().ok()?;
    let second: f64 = clock_parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second as i64 - offset)
}

/// "just now", "4 min ago", "2 h ago", "3 d ago".
pub fn ago(seconds: i64) -> String {
    match seconds {
        s if s < 60 => "just now".to_owned(),
        s if s < 3600 => format!("{} min ago", s / 60),
        s if s < 86_400 => format!("{} h ago", s / 3600),
        s => format!("{} d ago", s / 86_400),
    }
}

/// Howard Hinnant's algorithm: days since 1970-01-01 → (year, month, day).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let month = i64::from(month);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for seconds in [0, 951_782_400, 1_792_154_730, 4_102_444_800, -86_400] {
            assert_eq!(
                parse(&format(seconds)),
                Some(seconds),
                "{}",
                format(seconds)
            );
        }
        assert_eq!(format(0), "1970-01-01T00:00:00Z");
        assert_eq!(format(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn offsets() {
        assert_eq!(
            parse("2026-10-17T14:32:10+03:00"),
            parse("2026-10-17T11:32:10Z")
        );
        assert_eq!(
            parse("2026-10-17T08:32:10-03:00"),
            parse("2026-10-17T11:32:10Z")
        );
        assert_eq!(
            parse("2026-10-17T11:32:10.123Z"),
            parse("2026-10-17T11:32:10Z")
        );
        assert_eq!(parse("yesterday"), None);
    }

    #[test]
    fn ago_text() {
        assert_eq!(ago(5), "just now");
        assert_eq!(ago(300), "5 min ago");
        assert_eq!(ago(7200), "2 h ago");
        assert_eq!(ago(200_000), "2 d ago");
    }
}
