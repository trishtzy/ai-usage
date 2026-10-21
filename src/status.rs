//! Renders provider cards as text with progress bars, or as JSON.

use chrono::{DateTime, Datelike, TimeZone, Utc};
use serde_json::json;

use crate::usage::{Card, ago};

const MIN_BAR: usize = 10;
const MAX_BAR: usize = 40;
const INDENT: &str = "  ";
const GAP: &str = "  ";

pub struct Style {
    pub width: usize,
    pub color: bool,
}

impl Style {
    fn paint(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }
}

/// Renders `cards` for a terminal, showing times in `now`'s time zone.
pub fn render<Tz: TimeZone>(cards: &[Card], now: DateTime<Tz>, style: &Style) -> String
where
    Tz::Offset: std::fmt::Display,
{
    if cards.is_empty() {
        return "No AI usage found. Is Claude Code or Codex installed?\n".to_string();
    }
    let now_utc = now.to_utc();
    let detail_lines = |meter: &crate::usage::Meter| -> Vec<String> {
        let mut lines: Vec<String> = meter.detail.iter().cloned().collect();
        if let Some(resets_at) = meter.resets_at {
            lines.push(format!(
                "Resets {}",
                when(resets_at.with_timezone(&now.timezone()), &now)
            ));
        }
        lines
    };

    // One layout for every card, so the bars line up down the page.
    let meters = || cards.iter().flat_map(|card| &card.meters);
    let left = meters()
        .flat_map(|m| std::iter::once(m.label.clone()).chain(detail_lines(m)))
        .map(|text| text.chars().count())
        .max()
        .unwrap_or(0);
    let value = meters()
        .filter(|m| m.used_percent.is_some())
        .map(|m| m.value.chars().count())
        .max()
        .unwrap_or(0);
    let fixed = INDENT.len() + left + GAP.len() * 2 + value;
    let bar = style.width.saturating_sub(fixed).clamp(MIN_BAR, MAX_BAR);

    let mut out = String::new();
    for (i, card) in cards.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&style.paint("1", card.title));
        if let Some(as_of) = card.as_of {
            out.push_str(&style.paint("2", &format!(" · updated {}", ago(as_of, now_utc))));
        }
        out.push('\n');
        for meter in &card.meters {
            let mut line = format!("{INDENT}{:<left$}{GAP}", meter.label);
            match meter.used_percent {
                Some(percent) => {
                    line.push_str(&draw_bar(percent, bar, style));
                    line.push_str(GAP);
                    line.push_str(&format!("{:>value$}", meter.value));
                }
                None => line.push_str(&meter.value),
            }
            out.push_str(line.trim_end());
            out.push('\n');
            for detail in detail_lines(meter) {
                out.push_str(&format!("{INDENT}{}\n", style.paint("2", &detail)));
            }
        }
        for note in &card.notes {
            out.push_str(&format!("{INDENT}{}\n", style.paint("2", note)));
        }
    }
    out
}

/// A bar `width` cells wide, filled in eighths of a cell.
fn draw_bar(percent: f64, width: usize, style: &Style) -> String {
    const PARTS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    let eighths = ((percent.clamp(0.0, 100.0) / 100.0) * (width * 8) as f64).round() as usize;
    let (full, part) = (eighths / 8, eighths % 8);
    let mut filled = "█".repeat(full);
    let mut empty = width - full;
    if part > 0 {
        filled.push(PARTS[part]);
        empty -= 1;
    }
    let color = match percent {
        p if p >= 90.0 => "31",
        p if p >= 75.0 => "33",
        _ => "34",
    };
    format!(
        "{}{}",
        style.paint(color, &filled),
        style.paint("90", &"░".repeat(empty))
    )
}

/// A reset time the way claude.ai shows it: `Thu 1:50 AM`, with the date
/// added once it's a week or more away.
fn when<Tz: TimeZone>(at: DateTime<Tz>, now: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let days = (at.date_naive() - now.date_naive()).num_days();
    let time = at.format("%-I:%M %p");
    match days {
        0 => format!("today {time}"),
        1 => format!("tomorrow {time}"),
        2..7 => format!("{} {time}", at.format("%a")),
        _ if at.year() == now.year() => format!("{} {time}", at.format("%a %b %-d")),
        _ => format!("{} {time}", at.format("%a %b %-d %Y")),
    }
}

pub fn json(cards: &[Card], now: DateTime<Utc>) -> String {
    let value = json!({ "generated_at": now, "cards": cards });
    format!("{value:#}\n")
}

#[cfg(test)]
mod tests {
    use chrono::FixedOffset;

    use super::*;
    use crate::usage::Meter;

    fn now() -> DateTime<FixedOffset> {
        // Wed 21 Oct 2026, 11:00 PM in Singapore.
        "2026-10-21T23:00:00+08:00".parse().unwrap()
    }

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn cards() -> Vec<Card> {
        vec![
            Card {
                provider: "claude",
                title: "Claude Code",
                as_of: Some(at("2026-10-21T14:58:00Z")),
                meters: vec![
                    // Thu 1:50 AM and Mon 9:00 PM in Singapore.
                    Meter::limit(
                        "Current session".into(),
                        14.0,
                        Some(at("2026-10-21T17:50:00Z")),
                    ),
                    Meter::limit("This week".into(), 46.0, Some(at("2026-10-26T13:00:00Z"))),
                    Meter {
                        label: "Today".into(),
                        detail: Some("Input + output".into()),
                        used_percent: None,
                        value: "1.1M tokens".into(),
                        resets_at: None,
                    },
                ],
                notes: vec![],
            },
            Card {
                provider: "codex",
                title: "Codex",
                as_of: None,
                meters: vec![],
                notes: vec!["No plan limits reported in the last 30 days.".into()],
            },
        ]
    }

    #[test]
    fn renders_cards_with_bars() {
        let style = Style {
            width: 60,
            color: false,
        };
        let expected = "\
Claude Code · updated 2 min ago
  Current session          ███▎░░░░░░░░░░░░░░░░░░░  14% used
  Resets tomorrow 1:50 AM
  This week                ██████████▋░░░░░░░░░░░░  46% used
  Resets Mon 9:00 PM
  Today                    1.1M tokens
  Input + output

Codex
  No plan limits reported in the last 30 days.
";
        assert_eq!(render(&cards(), now(), &style), expected);
    }

    #[test]
    fn bar_width_follows_terminal_width() {
        let narrow = Style {
            width: 20,
            color: false,
        };
        let out = render(&cards(), now(), &narrow);
        let session = out.lines().nth(1).unwrap();
        assert!(session.contains("█▍░"), "{session}");
        assert_eq!(session.chars().filter(|&c| c == '░').count(), MIN_BAR - 2);
    }

    #[test]
    fn bars_fill_and_clamp() {
        let plain = Style {
            width: 80,
            color: false,
        };
        assert_eq!(draw_bar(0.0, 10, &plain), "░".repeat(10));
        assert_eq!(
            draw_bar(50.0, 10, &plain),
            format!("{}{}", "█".repeat(5), "░".repeat(5))
        );
        assert_eq!(draw_bar(100.0, 10, &plain), "█".repeat(10));
        assert_eq!(draw_bar(130.0, 10, &plain), "█".repeat(10));
        assert_eq!(draw_bar(1.0, 10, &plain), format!("▏{}", "░".repeat(9)));
    }

    #[test]
    fn colors_only_when_asked() {
        let out = render(
            &cards(),
            now(),
            &Style {
                width: 60,
                color: true,
            },
        );
        assert!(out.contains("\x1b[34m█"));
        let out = render(
            &cards(),
            now(),
            &Style {
                width: 60,
                color: false,
            },
        );
        assert!(!out.contains('\x1b'));
    }

    #[test]
    fn says_when_nothing_is_installed() {
        let out = render(
            &[],
            now(),
            &Style {
                width: 60,
                color: false,
            },
        );
        assert_eq!(
            out,
            "No AI usage found. Is Claude Code or Codex installed?\n"
        );
    }

    #[test]
    fn formats_reset_times() {
        let now = now();
        let tz = now.timezone();
        let local = |s: &str| at(s).with_timezone(&tz);
        assert_eq!(when(local("2026-10-21T15:30:00Z"), &now), "today 11:30 PM");
        assert_eq!(
            when(local("2026-10-21T17:50:00Z"), &now),
            "tomorrow 1:50 AM"
        );
        assert_eq!(when(local("2026-10-26T13:00:00Z"), &now), "Mon 9:00 PM");
        assert_eq!(
            when(local("2026-11-20T01:00:00Z"), &now),
            "Fri Nov 20 9:00 AM"
        );
        assert_eq!(
            when(local("2027-01-05T01:00:00Z"), &now),
            "Tue Jan 5 2027 9:00 AM"
        );
    }
}
