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
    /// Show the price ticker.
    Ticker,
    /// Show CPU/GPU/memory readings (and FPS in games).
    SystemInfo,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Segment {
    pub name: String,
    /// "daily", "weekdays", "weekends", day names like "mon", or ranges like "fri-sun"
    /// (empty = daily). "weekends" follows the weekend_days setting.
    pub days: Vec<String>,
    /// "HH:MM", 24-hour
    pub start: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<DisplayMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_mode: Option<crate::config::ConfigTimeMode>,
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

pub const ALL_DAYS: u8 = 0b111_1111;
pub const SAT_SUN: u8 = 0b110_0000;
pub const FRI_SUN: u8 = 0b111_0000;

const DAY_NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

fn day_index(name: &str) -> Option<u32> {
    let i = match name {
        "mon" | "monday" => 0,
        "tue" | "tues" | "tuesday" => 1,
        "wed" | "wednesday" => 2,
        "thu" | "thur" | "thurs" | "thursday" => 3,
        "fri" | "friday" => 4,
        "sat" | "saturday" => 5,
        "sun" | "sunday" => 6,
        _ => return None,
    };
    Some(i)
}

/// One day spec: a day ("fri"), a range ("fri-sun", may wrap: "sat-mon"), or a
/// keyword ("daily", "weekdays", "weekends").
fn parse_day_spec(spec: &str, weekend: u8) -> Result<u8, String> {
    let spec = spec.trim().to_ascii_lowercase();
    Ok(match spec.as_str() {
        "daily" | "all" | "everyday" => ALL_DAYS,
        "weekdays" => ALL_DAYS & !weekend,
        "weekends" => weekend,
        _ => {
            if let Some((a, b)) = spec.split_once('-') {
                let (Some(a), Some(b)) = (day_index(a.trim()), day_index(b.trim())) else {
                    return Err(format!("unknown day range \"{spec}\""));
                };
                let mut mask = 0u8;
                let mut i = a;
                loop {
                    mask |= 1 << i;
                    if i == b {
                        break;
                    }
                    i = (i + 1) % 7;
                }
                mask
            } else {
                1 << day_index(&spec).ok_or_else(|| format!("unknown day \"{spec}\""))?
            }
        }
    })
}

/// Lowercase day names as used in the config, Monday first.
pub const DAY_KEYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// The set of days (bit 0 = Monday) a `days` list means.
pub fn days_mask(days: &[String], weekend: u8) -> Result<u8, String> {
    parse_days(days, weekend)
}

fn parse_days(days: &[String], weekend: u8) -> Result<u8, String> {
    if days.is_empty() {
        return Ok(ALL_DAYS);
    }
    days.iter()
        .try_fold(0u8, |mask, d| Ok(mask | parse_day_spec(d, weekend)?))
}

/// The weekend_days setting, e.g. "sat-sun" or "fri-sun".
pub fn parse_weekend(spec: &str) -> Result<u8, String> {
    let mask = spec
        .split(',')
        .try_fold(0u8, |mask, part| {
            Ok::<u8, String>(mask | parse_day_spec(part, SAT_SUN)?)
        })
        .map_err(|e| format!("weekend_days: {e}"))?;
    if mask == 0 || mask == ALL_DAYS {
        return Err("weekend_days must include some days, but not all of them".into());
    }
    Ok(mask)
}

/// Readable form of a set of days: "Daily", "Mon–Thu", "Fri–Sun", "Mon, Wed".
pub fn days_label(mask: u8) -> String {
    if mask & ALL_DAYS == ALL_DAYS {
        return "Daily".into();
    }
    let mut runs: Vec<(usize, usize)> = vec![];
    for i in 0..7 {
        if mask & (1 << i) == 0 {
            continue;
        }
        match runs.last_mut() {
            Some((_, end)) if *end + 1 == i => *end = i,
            _ => runs.push((i, i)),
        }
    }
    // join a run ending on Sunday with one starting on Monday (e.g. Sat–Mon)
    if runs.len() > 1 && runs[0].0 == 0 && runs.last().unwrap().1 == 6 {
        let first = runs.remove(0);
        runs.last_mut().unwrap().1 = first.1;
    }
    runs.iter()
        .map(|&(a, b)| match (a, b) {
            _ if a == b => DAY_NAMES[a].to_string(),
            _ if (a + 1) % 7 == b => format!("{}, {}", DAY_NAMES[a], DAY_NAMES[b]),
            _ => format!("{}–{}", DAY_NAMES[a], DAY_NAMES[b]),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Check and parse every segment. Errors name the segment so they're easy to fix.
pub fn parse(segments: &[Segment], weekend: u8) -> Result<Vec<Timing>, String> {
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
            let days = parse_days(&seg.days, weekend).map_err(|e| format!("{label}: {e}"))?;
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
        parse(
            &[
                seg("Night", &["daily"], "00:00"),
                seg("Day", &["weekdays"], "08:00"),
                seg("Day", &["weekends"], "10:00"),
            ],
            SAT_SUN,
        )
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
        let t = parse(&[seg("Weekly", &["mon"], "12:00")], SAT_SUN).unwrap();
        assert_eq!(active(&t, at(2026, 11, 26, 3, 0)), Some(0));
        // Night-only schedule starting 22:00: just after midnight is still "night"
        let t = parse(&[seg("Late", &["daily"], "22:00")], SAT_SUN).unwrap();
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
        assert!(parse(&[seg("X", &["funday"], "08:00")], SAT_SUN).is_err());
        assert!(parse(&[seg("X", &[], "8am")], SAT_SUN).is_err());
        assert!(parse(&[seg("X", &[], "25:00")], SAT_SUN).is_err());
        assert!(parse(&[seg("X", &["fri-funday"], "08:00")], SAT_SUN).is_err());
        assert_eq!(parse(&[seg("X", &[], "8:05")], SAT_SUN).unwrap()[0].days, ALL_DAYS);
    }

    #[test]
    fn weekend_setting_and_ranges() {
        let segs = [seg("A", &["weekdays"], "08:00"), seg("B", &["weekends"], "10:00")];
        let t = parse(&segs, FRI_SUN).unwrap();
        assert_eq!(t[0].days, 0b000_1111); // Mon-Thu
        assert_eq!(t[1].days, FRI_SUN);
        // Friday 9:00 with a Fri-Sun weekend: weekday entry doesn't apply on Friday,
        // so Thursday's 08:00 "A" is still in effect until 10:00
        assert_eq!(active(&t, at(2026, 11, 20, 9, 0)), Some(0));
        assert_eq!(active(&t, at(2026, 11, 20, 10, 0)), Some(1));
        let r = parse(&[seg("W", &["sat-mon"], "08:00")], SAT_SUN).unwrap();
        assert_eq!(r[0].days, 0b110_0001);
        assert_eq!(parse_weekend("fri-sun").unwrap(), FRI_SUN);
        assert_eq!(parse_weekend("sat,sun").unwrap(), SAT_SUN);
        assert!(parse_weekend("daily").is_err());
    }

    #[test]
    fn labels() {
        assert_eq!(days_label(ALL_DAYS), "Daily");
        assert_eq!(days_label(0b000_1111), "Mon–Thu");
        assert_eq!(days_label(FRI_SUN), "Fri–Sun");
        assert_eq!(days_label(SAT_SUN), "Sat, Sun");
        assert_eq!(days_label(0b110_0001), "Sat–Mon");
        assert_eq!(days_label(0b000_0101), "Mon, Wed");
    }
}
