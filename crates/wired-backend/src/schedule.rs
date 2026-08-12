//! "Every morning at 8" — scheduling without cron.
//!
//! The 24/7 promise is only real if the assistant does something while nobody
//! is watching, and the previous answer to that was a crontab entry calling a
//! bash script. This is the same capability phrased as a sentence.
//!
//! Cron expressions still parse, because anyone who already thinks in them
//! should not be made to stop. Everyone else writes "every hour" or "every
//! weekday at 9am".

use std::str::FromStr;

use chrono::{
    DateTime, Datelike, Duration as ChronoDuration, Local, NaiveTime, TimeZone, Timelike, Weekday,
};
use serde::{Deserialize, Serialize};

// ── when ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// Fixed spacing from the last run.
    Every(i64),
    Daily {
        hour: u32,
        minute: u32,
    },
    Weekly {
        weekday: Weekday,
        hour: u32,
        minute: u32,
    },
    Cron(Cron),
}

/// Named times of day, so "every morning" means something specific and the UI
/// can say what it resolved to.
fn named_hour(word: &str) -> Option<u32> {
    Some(match word {
        "morning" => 8,
        "midday" | "noon" | "lunchtime" => 12,
        "afternoon" => 14,
        "evening" => 19,
        "night" | "midnight" => 22,
        _ => return None,
    })
}

fn weekday(word: &str) -> Option<Weekday> {
    Some(match word {
        "monday" | "mon" => Weekday::Mon,
        "tuesday" | "tue" | "tues" => Weekday::Tue,
        "wednesday" | "wed" => Weekday::Wed,
        "thursday" | "thu" | "thurs" => Weekday::Thu,
        "friday" | "fri" => Weekday::Fri,
        "saturday" | "sat" => Weekday::Sat,
        "sunday" | "sun" => Weekday::Sun,
        _ => return None,
    })
}

/// `8`, `8am`, `8:30`, `08:30`, `8:30 pm`, `20:00`.
fn parse_time(raw: &str) -> Option<(u32, u32)> {
    let text = raw.trim().to_ascii_lowercase().replace([' ', '.'], "");
    let (digits, shift) = if let Some(rest) = text.strip_suffix("am") {
        (rest, 0)
    } else if let Some(rest) = text.strip_suffix("pm") {
        (rest, 12)
    } else {
        (text.as_str(), -1)
    };

    let (hour, minute) = match digits.split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => (digits.parse::<u32>().ok()?, 0),
    };
    if minute > 59 {
        return None;
    }

    let hour = match shift {
        // "12am" is midnight and "12pm" is noon — the one case where adding
        // twelve is wrong in both directions.
        0 if hour == 12 => 0,
        0 if hour <= 11 => hour,
        12 if hour == 12 => 12,
        12 if hour <= 11 => hour + 12,
        -1 if hour <= 23 => hour,
        _ => return None,
    };
    Some((hour, minute))
}

fn parse_every(rest: &str) -> Option<Trigger> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    let (count, unit_index) = match words.first()?.parse::<i64>() {
        Ok(n) if n > 0 => (n, 1),
        // "every hour", "every morning", "every monday"
        _ => (1, 0),
    };
    let unit = words.get(unit_index)?.trim_end_matches('s');

    // "every day at 8", "every morning", "every monday at 9am"
    let at = words
        .iter()
        .position(|w| *w == "at")
        .and_then(|i| words.get(i + 1..))
        .map(|rest| rest.join(""))
        .and_then(|text| parse_time(&text));

    if let Some(day) = weekday(unit) {
        let (hour, minute) = at.unwrap_or((9, 0));
        return Some(Trigger::Weekly {
            weekday: day,
            hour,
            minute,
        });
    }
    if let Some(hour) = named_hour(unit) {
        let (hour, minute) = at.unwrap_or((hour, 0));
        return Some(Trigger::Daily { hour, minute });
    }

    match (unit, at) {
        ("day", Some((hour, minute))) if count == 1 => Some(Trigger::Daily { hour, minute }),
        ("minute", _) | ("min", _) => Some(Trigger::Every(count * 60)),
        ("hour", _) | ("hr", _) => Some(Trigger::Every(count * 3600)),
        ("day", _) => Some(Trigger::Every(count * 86_400)),
        ("week", _) => Some(Trigger::Every(count * 604_800)),
        _ => None,
    }
}

impl FromStr for Trigger {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let text = raw.trim().to_ascii_lowercase();
        if text.is_empty() {
            return Err("Say when it should run, e.g. \"every morning at 8\".".into());
        }

        if let Some(rest) = text.strip_prefix("every ") {
            if let Some(trigger) = parse_every(rest) {
                return Ok(trigger);
            }
        }
        for prefix in ["daily at ", "every day at ", "at "] {
            if let Some(rest) = text.strip_prefix(prefix) {
                if let Some((hour, minute)) = parse_time(rest) {
                    return Ok(Trigger::Daily { hour, minute });
                }
            }
        }
        if text == "hourly" {
            return Ok(Trigger::Every(3600));
        }
        if text == "daily" {
            return Ok(Trigger::Daily { hour: 9, minute: 0 });
        }
        if let Ok(cron) = Cron::parse(&text) {
            return Ok(Trigger::Cron(cron));
        }
        if let Some((hour, minute)) = parse_time(&text) {
            return Ok(Trigger::Daily { hour, minute });
        }

        Err(format!(
            "I don't understand \"{raw}\". Try \"every hour\", \"every morning at 8\", \
             \"every monday at 9am\", or a cron expression."
        ))
    }
}

impl Trigger {
    /// The first firing strictly after `from`.
    pub fn next_after(&self, from: DateTime<Local>) -> Option<DateTime<Local>> {
        match self {
            Trigger::Every(seconds) => Some(from + ChronoDuration::seconds(*seconds)),
            Trigger::Daily { hour, minute } => {
                let time = NaiveTime::from_hms_opt(*hour, *minute, 0)?;
                let today = from.date_naive().and_time(time);
                let candidate = to_local(today, from)?;
                Some(if candidate > from {
                    candidate
                } else {
                    to_local(today + ChronoDuration::days(1), from)?
                })
            }
            Trigger::Weekly {
                weekday,
                hour,
                minute,
            } => {
                let time = NaiveTime::from_hms_opt(*hour, *minute, 0)?;
                for ahead in 0..=7 {
                    let day = from.date_naive() + ChronoDuration::days(ahead);
                    if day.weekday() != *weekday {
                        continue;
                    }
                    let candidate = to_local(day.and_time(time), from)?;
                    if candidate > from {
                        return Some(candidate);
                    }
                }
                None
            }
            Trigger::Cron(cron) => cron.next_after(from),
        }
    }

    /// How the UI says it back, so the user can check we understood.
    pub fn describe(&self) -> String {
        match self {
            Trigger::Every(seconds) => match seconds {
                s if *s % 604_800 == 0 => plural(s / 604_800, "week"),
                s if *s % 86_400 == 0 => plural(s / 86_400, "day"),
                s if *s % 3600 == 0 => plural(s / 3600, "hour"),
                s => plural(s / 60, "minute"),
            },
            Trigger::Daily { hour, minute } => format!("every day at {hour:02}:{minute:02}"),
            Trigger::Weekly {
                weekday,
                hour,
                minute,
            } => format!("every {weekday:?} at {hour:02}:{minute:02}"),
            Trigger::Cron(cron) => format!("cron: {}", cron.source),
        }
    }
}

fn plural(count: i64, unit: &str) -> String {
    if count == 1 {
        format!("every {unit}")
    } else {
        format!("every {count} {unit}s")
    }
}

/// Local time is ambiguous twice a year. Around a DST jump, take whichever
/// reading exists rather than skipping the run entirely.
fn to_local(naive: chrono::NaiveDateTime, near: DateTime<Local>) -> Option<DateTime<Local>> {
    match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => Some(dt),
        chrono::LocalResult::Ambiguous(first, _) => Some(first),
        // The wall-clock time does not exist (spring forward): run an hour on.
        chrono::LocalResult::None => Local
            .from_local_datetime(&(naive + ChronoDuration::hours(1)))
            .single()
            .or(Some(near + ChronoDuration::hours(1))),
    }
}

// ── cron ────────────────────────────────────────────────────────────────

/// A five-field cron expression: minute hour day-of-month month day-of-week.
///
/// Small on purpose. It covers what people actually write, and anything it
/// refuses falls through to the sentence parser above, which produces a
/// readable error rather than a silent misfire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    source: String,
    minute: Vec<u32>,
    hour: Vec<u32>,
    day: Vec<u32>,
    month: Vec<u32>,
    weekday: Vec<u32>,
}

fn parse_field(raw: &str, min: u32, max: u32) -> Result<Vec<u32>, String> {
    let mut values = Vec::new();
    for part in raw.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => (range, step.parse::<u32>().map_err(|_| "bad step")?),
            None => (part, 1),
        };
        if step == 0 {
            return Err("step cannot be zero".into());
        }
        let (start, end) = if range == "*" {
            (min, max)
        } else if let Some((lo, hi)) = range.split_once('-') {
            (
                lo.parse::<u32>().map_err(|_| "bad range")?,
                hi.parse::<u32>().map_err(|_| "bad range")?,
            )
        } else {
            let value = range.parse::<u32>().map_err(|_| "bad number")?;
            (value, value)
        };
        if start < min || end > max || start > end {
            return Err("out of range".into());
        }
        values.extend((start..=end).step_by(step as usize));
    }
    values.sort_unstable();
    values.dedup();
    Ok(values)
}

impl Cron {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let fields: Vec<&str> = raw.split_whitespace().collect();
        if fields.len() != 5 {
            return Err("a cron expression has five fields".into());
        }
        Ok(Self {
            source: raw.trim().to_string(),
            minute: parse_field(fields[0], 0, 59)?,
            hour: parse_field(fields[1], 0, 23)?,
            day: parse_field(fields[2], 1, 31)?,
            month: parse_field(fields[3], 1, 12)?,
            // 7 and 0 are both Sunday, as every crontab has always allowed.
            weekday: parse_field(fields[4], 0, 7)?
                .into_iter()
                .map(|d| d % 7)
                .collect(),
        })
    }

    fn matches(&self, at: DateTime<Local>) -> bool {
        let weekday = at.weekday().num_days_from_sunday();
        self.minute.contains(&at.minute())
            && self.hour.contains(&at.hour())
            && self.month.contains(&at.month())
            // Standard cron: with both day-of-month and day-of-week restricted,
            // either matching is enough.
            && match (self.day.len() == 31, self.weekday.len() >= 7) {
                (false, false) => self.day.contains(&at.day()) || self.weekday.contains(&weekday),
                _ => self.day.contains(&at.day()) && self.weekday.contains(&weekday),
            }
    }

    fn next_after(&self, from: DateTime<Local>) -> Option<DateTime<Local>> {
        let mut at = (from + ChronoDuration::minutes(1))
            .with_second(0)?
            .with_nanosecond(0)?;
        // Four years of minutes is enough to find any expression that can fire
        // at all, including 29 February.
        for _ in 0..(60 * 24 * 366 * 4) {
            if self.matches(at) {
                return Some(at);
            }
            at += ChronoDuration::minutes(1);
        }
        None
    }
}

// ── the schedule itself ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    pub id: String,
    pub name: String,
    pub task: String,
    /// The sentence the user wrote: "every morning at 8".
    pub when: String,
    pub enabled: bool,
    /// Add the "[SILENT] if there is nothing to say" instruction, so a monitor
    /// that finds nothing sends nothing.
    pub quiet_when_nothing: bool,
    pub last_run: Option<f64>,
    pub last_result: Option<String>,
    pub next_run: Option<f64>,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            task: String::new(),
            when: String::new(),
            enabled: true,
            quiet_when_nothing: true,
            last_run: None,
            last_result: None,
            next_run: None,
        }
    }
}

impl Schedule {
    pub fn trigger(&self) -> Result<Trigger, String> {
        self.when.parse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn understands_sentences() {
        assert_eq!(
            "every hour".parse::<Trigger>().unwrap(),
            Trigger::Every(3600)
        );
        assert_eq!(
            "every 30 minutes".parse::<Trigger>().unwrap(),
            Trigger::Every(1800)
        );
        assert_eq!(
            "every morning at 8".parse::<Trigger>().unwrap(),
            Trigger::Daily { hour: 8, minute: 0 }
        );
        assert_eq!(
            "every evening".parse::<Trigger>().unwrap(),
            Trigger::Daily {
                hour: 19,
                minute: 0
            }
        );
        assert_eq!(
            "every monday at 9am".parse::<Trigger>().unwrap(),
            Trigger::Weekly {
                weekday: Weekday::Mon,
                hour: 9,
                minute: 0
            }
        );
        assert_eq!(
            "at 6:30 pm".parse::<Trigger>().unwrap(),
            Trigger::Daily {
                hour: 18,
                minute: 30
            }
        );
    }

    #[test]
    fn understands_cron() {
        let Trigger::Cron(cron) = "0 8 * * 1-5".parse::<Trigger>().unwrap() else {
            panic!("not parsed as cron");
        };
        assert_eq!(cron.hour, vec![8]);
        assert_eq!(cron.weekday, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn rejects_nonsense_with_an_example() {
        let err = "whenever".parse::<Trigger>().unwrap_err();
        assert!(err.contains("every morning at 8"), "got: {err}");
    }

    #[test]
    fn midnight_and_noon_are_not_off_by_twelve() {
        assert_eq!(parse_time("12am"), Some((0, 0)));
        assert_eq!(parse_time("12pm"), Some((12, 0)));
        assert_eq!(parse_time("11pm"), Some((23, 0)));
    }

    #[test]
    fn daily_never_returns_the_past() {
        let now = Local::now();
        let trigger = Trigger::Daily {
            hour: now.hour(),
            minute: now.minute(),
        };
        assert!(trigger.next_after(now).unwrap() > now);
    }
}
