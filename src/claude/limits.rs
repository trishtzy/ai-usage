//! Claude plan limits. Claude Code passes them to its status line command
//! and stores them nowhere else, so `ai-usage claude-statusline` saves each
//! update for `ai-usage status` to read.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::usage::{Meter, window_label};

#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct RateLimits {
    five_hour: Option<Window>,
    seven_day: Option<Window>,
    spend_limit: Option<SpendLimit>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
struct Window {
    used_percentage: f64,
    /// Unix seconds.
    resets_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
struct SpendLimit {
    used_percentage: f64,
    resets_at: i64,
    used_usd: Option<f64>,
    limit_usd: Option<f64>,
    period: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
pub struct Snapshot {
    pub as_of: DateTime<Utc>,
    rate_limits: RateLimits,
}

#[derive(Deserialize)]
struct StatusLineInput {
    rate_limits: Option<RateLimits>,
}

/// `$XDG_STATE_HOME/ai-usage/claude-rate-limits.json`, with
/// `XDG_STATE_HOME` defaulting to `~/.local/state`.
pub fn default_path() -> Option<PathBuf> {
    let state = match std::env::var_os("XDG_STATE_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".local/state"),
    };
    Some(state.join("ai-usage/claude-rate-limits.json"))
}

/// Handles one status line update: saves the limits in `input`, if it has
/// any, and returns the line to show, such as `5h 14% · 7d 46%`. Never
/// fails, since a status line that errors is worse than a blank one.
pub fn statusline(input: &str, path: Option<&Path>, now: DateTime<Utc>) -> String {
    let Ok(StatusLineInput {
        rate_limits: Some(rate_limits),
    }) = serde_json::from_str(input)
    else {
        return String::new();
    };
    let snapshot = Snapshot {
        as_of: now,
        rate_limits,
    };
    if let Some(path) = path {
        let _ = save(path, &snapshot);
    }
    let limits = &snapshot.rate_limits;
    let mut parts = Vec::new();
    for (name, window) in [("5h", limits.five_hour), ("7d", limits.seven_day)] {
        if let Some(w) = window.filter(|w| w.resets_at > now.timestamp()) {
            parts.push(format!("{name} {:.0}%", w.used_percentage));
        }
    }
    if let Some(spend) = limits
        .spend_limit
        .as_ref()
        .filter(|s| s.resets_at > now.timestamp())
    {
        parts.push(format!("spend {:.0}%", spend.used_percentage));
    }
    parts.join(" · ")
}

/// Writes through a temporary file, so a reader never sees half of it.
fn save(path: &Path, snapshot: &Snapshot) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    fs::write(&tmp, serde_json::to_vec(snapshot)?)?;
    fs::rename(&tmp, path)
}

pub fn load(path: &Path) -> Option<Snapshot> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// The limits in `snapshot` whose window hasn't reset by `now`. A window
/// that has reset tells us nothing about the new one.
pub fn meters(snapshot: &Snapshot, now: DateTime<Utc>) -> Vec<Meter> {
    let limits = &snapshot.rate_limits;
    let mut meters = Vec::new();
    for (minutes, window) in [(300, limits.five_hour), (10_080, limits.seven_day)] {
        let Some(w) = window else { continue };
        let Some(resets_at) = DateTime::from_timestamp(w.resets_at, 0).filter(|&t| t > now) else {
            continue;
        };
        meters.push(Meter::limit(
            window_label(minutes),
            w.used_percentage,
            Some(resets_at),
        ));
    }
    if let Some(spend) = &limits.spend_limit
        && let Some(resets_at) = DateTime::from_timestamp(spend.resets_at, 0).filter(|&t| t > now)
    {
        let mut meter = Meter::limit(
            "Spend limit".to_string(),
            spend.used_percentage,
            Some(resets_at),
        );
        meter.detail = spend
            .period
            .as_ref()
            .map(|period| format!("{period} limit"));
        if let (Some(used), Some(limit)) = (spend.used_usd, spend.limit_usd) {
            meter.value = format!("${used:.2} of ${limit:.2}");
        }
        meters.push(meter);
    }
    meters
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        "2026-10-21T12:00:00Z".parse().unwrap()
    }

    // Seconds after `now()`.
    fn later(seconds: i64) -> i64 {
        now().timestamp() + seconds
    }

    #[test]
    fn saves_limits_and_shows_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/limits.json");
        let input = format!(
            r#"{{"model":{{"id":"claude-opus-5-5"}},"rate_limits":{{"five_hour":{{"used_percentage":14.2,"resets_at":{}}},"seven_day":{{"used_percentage":46,"resets_at":{}}}}}}}"#,
            later(3_600),
            later(86_400)
        );

        assert_eq!(statusline(&input, Some(&path), now()), "5h 14% · 7d 46%");

        let snapshot = load(&path).unwrap();
        assert_eq!(snapshot.as_of, now());
        let got: Vec<_> = meters(&snapshot, now())
            .into_iter()
            .map(|m| (m.label, m.value))
            .collect();
        assert_eq!(
            got,
            [
                ("Current session".to_string(), "14% used".to_string()),
                ("This week".to_string(), "46% used".to_string()),
            ]
        );
    }

    #[test]
    fn keeps_old_limits_when_an_update_has_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("limits.json");
        let with = format!(
            r#"{{"rate_limits":{{"five_hour":{{"used_percentage":5,"resets_at":{}}}}}}}"#,
            later(60)
        );
        statusline(&with, Some(&path), now());

        for input in ["", "not json", r#"{"model":{}}"#] {
            assert_eq!(statusline(input, Some(&path), now()), "");
        }
        assert_eq!(load(&path).unwrap().as_of, now());
    }

    #[test]
    fn drops_windows_that_have_reset() {
        let snapshot = Snapshot {
            as_of: now(),
            rate_limits: RateLimits {
                five_hour: Some(Window {
                    used_percentage: 90.0,
                    resets_at: later(-1),
                }),
                seven_day: Some(Window {
                    used_percentage: 46.0,
                    resets_at: later(1),
                }),
                spend_limit: None,
            },
        };
        let labels: Vec<_> = meters(&snapshot, now())
            .into_iter()
            .map(|m| m.label)
            .collect();
        assert_eq!(labels, ["This week"]);
    }

    #[test]
    fn shows_spend_in_dollars() {
        let snapshot = Snapshot {
            as_of: now(),
            rate_limits: RateLimits {
                spend_limit: Some(SpendLimit {
                    used_percentage: 54.0,
                    resets_at: later(60),
                    used_usd: Some(27.0),
                    limit_usd: Some(50.0),
                    period: Some("monthly".to_string()),
                }),
                ..Default::default()
            },
        };
        let meter = meters(&snapshot, now()).remove(0);
        assert_eq!(meter.value, "$27.00 of $50.00");
        assert_eq!(meter.detail.as_deref(), Some("monthly limit"));
        assert_eq!(meter.used_percent, Some(54.0));
    }
}
