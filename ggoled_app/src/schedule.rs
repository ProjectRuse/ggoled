//! Time-of-day schedule: a list of segments, each starting at a time on certain
//! days and lasting until the next segment starts. A segment overrides any of
//! the app's settings it specifies; anything it leaves out falls back to the
//! normal (tray) settings.

use chrono::{DateTime, Datelike, Days, Local, NaiveDate, NaiveTime, TimeZone};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum DisplayMode {
    /// Draw whatever is enabled (time, media, ...), as usual.
    #[default]
    Normal,
    /// Keep the OLED dark (volume changes still show briefly).
    Blank,
    /// Leave the screen to the SteelSeries UI.
    Stock,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Segment {
    pub name: String,
    /// "daily", "weekdays", "weekends", or day names like "mon", "tue" (empty = daily)
    pub days: Vec<String>,
    /// "HH:MM", 24-hour
    pub start: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<DisplayMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_mode: Option<crate::ConfigTimeMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_media: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screensaver: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screensaver_secs: Option<u32>,
    /// OLED brightness, 1-10
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brightness: Option<u8>,
    /// Scarlett front-panel LED sleep time in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scarlett_led_timeout: Option<u32>,
}

/// A segment's parsed timing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timing {
    /// bit 0 = Monday ... bit 6 = Sunday
    pub days: u8,
    pub start: NaiveTime,
}

const ALL_DAYS: u8 = 0b111_1111;

fn parse_days(days: &[String]) -> Result<u8, String> {
    if days.is_empty() {
        return Ok(ALL_DAYS);
    }
    let mut mask = 0u8;
    for d in days {
        mask |= match d.trim().to_ascii_lowercase().as_str() {
            "daily" | "all" | "everyday" => ALL_DAYS,
            "weekdays" => 0b001_1111,
            "weekends" => 0b110_0000,
            "mon" | "monday" => 1 << 0,
            "tue" | "tuesday" => 1 << 1,
            "wed" | "wednesday" => 1 << 2,
            "thu" | "thursday" => 1 << 3,
            "fri" | "friday" => 1 << 4,
            "sat" | "saturday" => 1 << 5,
            "sun" | "sunday" => 1 << 6,
            other => return Err(format!("unknown day \"{other}\"")),
        };
    }
    Ok(mask)
}

/// Check and parse every segment. Errors name the segment so they're easy to fix.
pub fn parse(segments: &[Segment]) -> Result<Vec<Timing>, String> {
    segments
        .iter()
        .enumerate()
        .map(|(i, seg)| {
            let label = if seg.name.is_empty() {
                format!("schedule entry #{}", i + 1)
            } else {
                format!("schedule entry \"{}\"", seg.name)
            };
            let start = NaiveTime::parse_from_str(seg.start.trim(), "%H:%M")
                .map_err(|_| format!("{label}: start \"{}\" should look like \"08:00\"", seg.start))?;
            let days = parse_days(&seg.days).map_err(|e| format!("{label}: {e}"))?;
            if let Some(b) = seg.brightness
                && !(1..=10).contains(&b)
            {
                return Err(format!("{label}: brightness must be 1-10"));
            }
            Ok(Timing { days, start })
        })
        .collect()
}

fn on_day(t: &Timing, date: NaiveDate) -> bool {
    t.days & (1 << date.weekday().num_days_from_monday()) != 0
}

fn local_at(date: NaiveDate, time: NaiveTime) -> Option<DateTime<Local>> {
    // `earliest` handles DST transitions (skipped/repeated hours)
    Local.from_local_datetime(&date.and_time(time)).earliest()
}

/// Index of the segment in effect at `now`: the one that most recently started.
pub fn active(timings: &[Timing], now: DateTime<Local>) -> Option<usize> {
    let today = now.date_naive();
    for back in 0..=7 {
        let date = today.checked_sub_days(Days::new(back))?;
        let best = timings
            .iter()
            .enumerate()
            .filter(|(_, t)| on_day(t, date) && (back > 0 || t.start <= now.time()))
            // latest start that day wins; on a tie, the later entry in the list wins
            .max_by_key(|(i, t)| (t.start, *i));
        if let Some((i, _)) = best {
            return Some(i);
        }
    }
    None
}

/// When the next segment starts (strictly after `now`).
pub fn next_boundary(timings: &[Timing], now: DateTime<Local>) -> Option<DateTime<Local>> {
    let today = now.date_naive();
    for ahead in 0..=7 {
        let date = today.checked_add_days(Days::new(ahead))?;
        let next = timings
            .iter()
            .filter(|t| on_day(t, date))
            .filter_map(|t| local_at(date, t.start))
            .filter(|dt| *dt > now)
            .min();
        if next.is_some() {
            return next;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(name: &str, days: &[&str], start: &str) -> Segment {
        Segment {
            name: name.into(),
            days: days.iter().map(|s| s.to_string()).collect(),
            start: start.into(),
            ..Default::default()
        }
    }

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, h, min, 0).earliest().unwrap()
    }

    // 2026-11-20 is a Friday, 2026-11-21 Saturday, 2026-11-23 Monday
    fn sample() -> Vec<Timing> {
        parse(&[
            seg("Night", &["daily"], "00:00"),
            seg("Day", &["weekdays"], "08:00"),
            seg("Day", &["weekends"], "10:00"),
        ])
        .unwrap()
    }

    #[test]
    fn picks_current_segment() {
        let t = sample();
        assert_eq!(active(&t, at(2026, 11, 20, 7, 59)), Some(0)); // Fri early: night
        assert_eq!(active(&t, at(2026, 11, 20, 8, 0)), Some(1)); // Fri 8:00: day
        assert_eq!(active(&t, at(2026, 11, 20, 23, 59)), Some(1));
        assert_eq!(active(&t, at(2026, 11, 21, 9, 0)), Some(0)); // Sat 9:00: still night
        assert_eq!(active(&t, at(2026, 11, 21, 10, 0)), Some(2)); // Sat 10:00: weekend day
    }

    #[test]
    fn carries_over_from_previous_days() {
        // Only one segment, starting Mondays: on Thursday it's still in effect.
        let t = parse(&[seg("Weekly", &["mon"], "12:00")]).unwrap();
        assert_eq!(active(&t, at(2026, 11, 26, 3, 0)), Some(0));
        // Night-only schedule starting 22:00: just after midnight is still "night"
        let t = parse(&[seg("Late", &["daily"], "22:00")]).unwrap();
        assert_eq!(active(&t, at(2026, 11, 20, 1, 0)), Some(0));
    }

    #[test]
    fn next_boundary_works() {
        let t = sample();
        assert_eq!(next_boundary(&t, at(2026, 11, 20, 2, 0)), Some(at(2026, 11, 20, 8, 0)));
        assert_eq!(next_boundary(&t, at(2026, 11, 20, 8, 0)), Some(at(2026, 11, 21, 0, 0)));
        assert_eq!(
            next_boundary(&t, at(2026, 11, 21, 0, 30)),
            Some(at(2026, 11, 21, 10, 0))
        );
        assert_eq!(next_boundary(&[], at(2026, 11, 21, 0, 30)), None);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse(&[seg("X", &["funday"], "08:00")]).is_err());
        assert!(parse(&[seg("X", &[], "8am")]).is_err());
        assert!(parse(&[seg("X", &[], "25:00")]).is_err());
        assert_eq!(parse(&[seg("X", &[], "8:05")]).unwrap()[0].days, ALL_DAYS);
    }
}
