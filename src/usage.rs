use std::ops::AddAssign;

use chrono::{DateTime, Utc};
use serde::Serialize;

/// Token counts for one or more model responses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

impl Tokens {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_write + self.cache_read
    }
}

impl AddAssign for Tokens {
    fn add_assign(&mut self, other: Self) {
        self.input += other.input;
        self.output += other.output;
        self.cache_write += other.cache_write;
        self.cache_read += other.cache_read;
    }
}

/// One model response, as read from a provider's logs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub provider: &'static str,
    pub model: String,
    pub timestamp: DateTime<Utc>,
    pub tokens: Tokens,
}

/// One provider's section of `ai-usage status`. Each provider fills in the
/// meters it knows about, so they needn't share the same figures.
#[derive(Debug, PartialEq, Serialize)]
pub struct Card {
    pub provider: &'static str,
    pub title: &'static str,
    /// When the provider last reported its limits.
    pub as_of: Option<DateTime<Utc>>,
    pub meters: Vec<Meter>,
    pub notes: Vec<String>,
}

/// One figure on a card: a limit with a bar, or a plain count without one.
#[derive(Debug, PartialEq, Serialize)]
pub struct Meter {
    pub label: String,
    pub detail: Option<String>,
    /// Share of a limit used, from 0 up. `None` shows no bar.
    pub used_percent: Option<f64>,
    pub value: String,
    pub resets_at: Option<DateTime<Utc>>,
}

impl Meter {
    pub fn limit(label: String, used_percent: f64, resets_at: Option<DateTime<Utc>>) -> Self {
        Meter {
            label,
            detail: None,
            used_percent: Some(used_percent),
            value: format!("{used_percent:.0}% used"),
            resets_at,
        }
    }

    /// Input and output tokens used today, as a figure with no bar.
    pub fn today(tokens: u64) -> Self {
        Meter {
            label: "Today".to_string(),
            detail: None,
            used_percent: None,
            value: format!("{} tokens", compact(tokens)),
            resets_at: None,
        }
    }
}

/// Names a usage window the way claude.ai does, such as `This week`.
pub fn window_label(minutes: u64) -> String {
    match minutes {
        300 => "Current session".to_string(),
        10_080 => "This week".to_string(),
        43_200 => "This month".to_string(),
        m if m % 1_440 == 0 => format!("{}-day window", m / 1_440),
        m if m % 60 == 0 => format!("{}-hour window", m / 60),
        m => format!("{m}-minute window"),
    }
}

/// How long before `now` something happened, such as `5 min ago`.
pub fn ago(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let minutes = (now - then).num_minutes();
    match minutes {
        ..1 => "just now".to_string(),
        1..60 => format!("{minutes} min ago"),
        60..1_440 => format!("{} h ago", minutes / 60),
        _ => format!("{} days ago", minutes / 1_440),
    }
}

/// 1234567 → `1.2M`.
pub fn compact(n: u64) -> String {
    const UNITS: [(u64, &str); 3] = [(1_000_000_000, "B"), (1_000_000, "M"), (1_000, "K")];
    for (size, unit) in UNITS {
        if n >= size {
            return format!("{:.1}{unit}", n as f64 / size as f64);
        }
    }
    n.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_windows() {
        assert_eq!(window_label(300), "Current session");
        assert_eq!(window_label(10_080), "This week");
        assert_eq!(window_label(43_200), "This month");
        assert_eq!(window_label(2_880), "2-day window");
        assert_eq!(window_label(120), "2-hour window");
        assert_eq!(window_label(90), "90-minute window");
    }

    #[test]
    fn says_how_long_ago() {
        let now: DateTime<Utc> = "2026-10-21T12:00:00Z".parse().unwrap();
        let at = |s: &str| s.parse::<DateTime<Utc>>().unwrap();
        assert_eq!(ago(at("2026-10-21T11:59:30Z"), now), "just now");
        assert_eq!(ago(at("2026-10-21T11:55:00Z"), now), "5 min ago");
        assert_eq!(ago(at("2026-10-21T09:00:00Z"), now), "3 h ago");
        assert_eq!(ago(at("2026-06-09T02:21:50Z"), now), "134 days ago");
    }

    #[test]
    fn compacts_numbers() {
        assert_eq!(compact(999), "999");
        assert_eq!(compact(12_345), "12.3K");
        assert_eq!(compact(3_000_000_000), "3.0B");
    }
}
