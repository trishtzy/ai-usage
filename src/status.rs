//! Renders provider cards as text with progress bars, or as JSON.

use chrono::{DateTime, Datelike, TimeZone, Utc};
use serde_json::json;

use crate::usage::{Card, Meter, ago};

/// Below this many columns, cards use the narrow, stacked layout.
const STACK_BELOW: usize = 64;
const MIN_BAR: usize = 10;
const MAX_BAR: usize = 40;
const INDENT: &str = "  ";
const GAP: &str = "  ";

pub struct Style {
    pub width: usize,
    pub color: bool,
    /// SGR parameters for a background to paint under everything, such as
    /// `40`. Rex's floating panes show the pane beneath through any cell
    /// that has no background of its own.
    pub background: Option<String>,
}

impl Style {
    pub fn paint(&self, code: &str, text: &str) -> String {
        if !self.color {
            return text.to_string();
        }
        // A reset clears the background too, so put it back.
        let restore = self
            .background
            .as_ref()
            .map_or(String::new(), |bg| format!("\x1b[{bg}m"));
        format!("\x1b[{code}m{text}\x1b[0m{restore}")
    }

    /// Paints `text` onto a `width` by `height` background, so no cell is
    /// left see-through. Without a background, returns `text` unchanged.
    pub fn fill(&self, text: &str, height: usize) -> String {
        let Some(bg) = self.background.as_ref().filter(|_| self.color) else {
            return text.to_string();
        };
        let mut lines: Vec<String> = text
            .lines()
            .map(|line| {
                let pad = self.width.saturating_sub(visible_width(line));
                format!("\x1b[{bg}m{line}{}\x1b[0m", " ".repeat(pad))
            })
            .collect();
        while lines.len() < height {
            lines.push(format!("\x1b[{bg}m{}\x1b[0m", " ".repeat(self.width)));
        }
        lines.join("\n")
    }
}

impl Style {
    /// Draws a rounded border `self.width` wide and `height` tall around
    /// `body`, with a blank row above it and `footer` on the bottom row.
    /// Without a height, the box ends just below the body.
    pub fn boxed(&self, body: &str, footer: &str, height: usize) -> String {
        let inner = self.width.saturating_sub(2);
        let side = self.paint("90", "│");
        let edge = |left: char, right: char| {
            self.paint("90", &format!("{left}{}{right}", "─".repeat(inner)))
        };
        let mut rows: Vec<&str> = std::iter::once("").chain(body.lines()).collect();
        let fit = height.saturating_sub(2);
        if fit > 0 {
            // Keep the bottom edge and footer on screen, losing the body's end.
            rows.truncate(fit.saturating_sub(1));
            rows.resize(fit - 1, "");
        } else {
            rows.push("");
        }
        rows.push(footer);

        let mut lines = vec![edge('╭', '╮')];
        for row in rows {
            let pad = inner.saturating_sub(visible_width(row));
            lines.push(format!("{side}{row}{}{side}", " ".repeat(pad)));
        }
        lines.push(edge('╰', '╯'));
        lines.join("\n")
    }
}

/// Parses `--background`: `black` for the theme's palette black, `#rrggbb`,
/// or `none` to leave cells see-through.
pub fn parse_background(s: &str) -> Result<Option<String>, String> {
    match s {
        "none" => return Ok(None),
        "black" => return Ok(Some("40".to_string())),
        _ => {}
    }
    let hex = s
        .strip_prefix('#')
        .filter(|h| h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit()));
    let Some(hex) = hex else {
        return Err(format!("expected black, none, or #rrggbb, got {s:?}"));
    };
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0);
    Ok(Some(format!(
        "48;2;{};{};{}",
        channel(0),
        channel(2),
        channel(4)
    )))
}

/// Columns `line` takes up, leaving out escape sequences.
fn visible_width(line: &str) -> usize {
    let mut width = 0;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip to the final byte of a CSI sequence, such as `m`.
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            width += 1;
        }
    }
    width
}

/// Renders `cards` for a terminal, showing times in `now`'s time zone.
pub fn render<Tz: TimeZone>(cards: &[Card], now: DateTime<Tz>, style: &Style) -> String
where
    Tz::Offset: std::fmt::Display,
{
    if cards.is_empty() {
        return "No AI usage found. Is Claude Code or Codex installed?\n".to_string();
    }
    if style.width < STACK_BELOW {
        return render_stacked(cards, now, style);
    }
    let now_utc = now.to_utc();
    let detail_lines = |meter: &Meter| detail_lines(meter, &now);

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
        out.push_str(&heading(card, style));
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

/// The narrow layout, for a pane in a corner: each meter's label and value
/// on one line, its bar the full width beneath, and its details below that.
fn render_stacked<Tz: TimeZone>(cards: &[Card], now: DateTime<Tz>, style: &Style) -> String
where
    Tz::Offset: std::fmt::Display,
{
    // The same margin on both sides, so the card looks centred in its pane.
    let inner = style.width.saturating_sub(INDENT.len() * 2).max(MIN_BAR);
    let dim = |out: &mut String, text: &str| {
        for line in wrap(text, inner) {
            out.push_str(&format!("{INDENT}{}\n", style.paint("2", &line)));
        }
    };
    let mut out = String::new();
    for (i, card) in cards.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(INDENT);
        out.push_str(&heading(card, style));
        out.push('\n');
        if let Some(as_of) = card.as_of {
            dim(&mut out, &format!("Updated {}", ago(as_of, now.to_utc())));
        }
        for meter in &card.meters {
            out.push('\n');
            let used = meter.label.chars().count() + meter.value.chars().count();
            let pad = inner.saturating_sub(used).max(1);
            out.push_str(&format!(
                "{INDENT}{}{}{}\n",
                meter.label,
                " ".repeat(pad),
                meter.value
            ));
            if let Some(percent) = meter.used_percent {
                out.push_str(&format!("{INDENT}{}\n", draw_bar(percent, inner, style)));
            }
            for detail in detail_lines(meter, &now) {
                dim(&mut out, &detail);
            }
        }
        for note in &card.notes {
            if !card.meters.is_empty() {
                out.push('\n');
            }
            dim(&mut out, note);
        }
    }
    out
}

/// A card's title after its provider's mark, such as `✻ Claude Code`.
fn heading(card: &Card, style: &Style) -> String {
    let (mark, color) = mark(card.provider);
    format!(
        "{} {}",
        style.paint(color, mark),
        style.paint("1", card.title)
    )
}

/// A provider's mark and the SGR parameters to draw it with.
pub fn mark(provider: &str) -> (&'static str, &'static str) {
    match provider {
        // Claude's spark, in Claude's orange.
        "claude" => ("✻", "38;2;217;119;87"),
        "codex" => ("⬡", "1"),
        "opencode" => ("▣", "1"),
        _ => ("●", "2"),
    }
}

/// The lines under a meter: its detail, then when it resets.
fn detail_lines<Tz: TimeZone>(meter: &Meter, now: &DateTime<Tz>) -> Vec<String>
where
    Tz::Offset: std::fmt::Display,
{
    let mut lines: Vec<String> = meter.detail.iter().cloned().collect();
    if let Some(resets_at) = meter.resets_at {
        lines.push(format!(
            "Resets {}",
            when(resets_at.with_timezone(&now.timezone()), now)
        ));
    }
    lines
}

/// Breaks `text` into lines of at most `width` columns, between words.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
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
                        detail: None,
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

    /// `full` whole cells, then `part`, then `empty` empty cells.
    fn bar(full: usize, part: &str, empty: usize) -> String {
        format!("{}{part}{}", "█".repeat(full), "░".repeat(empty))
    }

    #[test]

    fn renders_wide_cards_with_bars_beside_labels() {
        let style = Style {
            width: 70,
            color: false,
            background: None,
        };
        let expected = [
            "✻ Claude Code · updated 2 min ago".to_string(),
            format!("  Current session          {}  14% used", bar(4, "▋", 28)),
            "  Resets tomorrow 1:50 AM".to_string(),
            format!("  This week                {}  46% used", bar(15, "▏", 17)),
            "  Resets Mon 9:00 PM".to_string(),
            "  Today                    1.1M tokens".to_string(),
            String::new(),
            "⬡ Codex".to_string(),
            "  No plan limits reported in the last 30 days.".to_string(),
        ];
        let out = render(&cards(), now(), &style);
        assert_eq!(out.lines().collect::<Vec<_>>(), expected);
    }

    #[test]
    fn renders_narrow_cards_with_bars_beneath_labels() {
        let style = Style {
            width: 34,
            color: false,
            background: None,
        };
        let expected = [
            "  ✻ Claude Code".to_string(),
            "  Updated 2 min ago".to_string(),
            String::new(),
            "  Current session       14% used".to_string(),
            format!("  {}", bar(4, "▎", 25)),
            "  Resets tomorrow 1:50 AM".to_string(),
            String::new(),
            "  This week             46% used".to_string(),
            format!("  {}", bar(13, "▊", 16)),
            "  Resets Mon 9:00 PM".to_string(),
            String::new(),
            "  Today              1.1M tokens".to_string(),
            String::new(),
            "  ⬡ Codex".to_string(),
            "  No plan limits reported in the".to_string(),
            "  last 30 days.".to_string(),
        ];
        let out = render(&cards(), now(), &style);
        assert_eq!(out.lines().collect::<Vec<_>>(), expected);
    }

    #[test]
    fn bars_never_shrink_below_the_minimum() {
        let tiny = Style {
            width: 5,
            color: false,
            background: None,
        };
        let out = render(&cards(), now(), &tiny);
        let bar = out.lines().find(|l| l.contains('░')).unwrap().trim();
        assert_eq!(bar.chars().count(), MIN_BAR, "{bar}");
    }

    #[test]
    fn wraps_between_words() {
        assert_eq!(wrap("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap("unbreakable word", 5), ["unbreakable", "word"]);
        assert!(wrap("   ", 5).is_empty());
    }

    #[test]
    fn bars_fill_and_clamp() {
        let plain = Style {
            width: 80,
            color: false,
            background: None,
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
                background: None,
            },
        );
        assert!(out.contains("\x1b[34m█"));
        let out = render(
            &cards(),
            now(),
            &Style {
                width: 60,
                color: false,
                background: None,
            },
        );
        assert!(!out.contains('\x1b'));
    }

    #[test]
    fn fills_every_cell_with_the_background() {
        let style = Style {
            width: 6,
            color: true,
            background: Some("40".into()),
        };
        let text = format!("ab{}\nc", style.paint("34", "x"));
        let lines: Vec<_> = style.fill(&text, 3).lines().map(String::from).collect();
        assert_eq!(
            lines,
            [
                // The reset after the blue x puts the background back.
                "\x1b[40mab\x1b[34mx\x1b[0m\x1b[40m   \x1b[0m",
                "\x1b[40mc     \x1b[0m",
                "\x1b[40m      \x1b[0m",
            ]
        );
    }

    #[test]
    fn draws_a_border_with_the_footer_on_the_bottom_row() {
        let style = Style {
            width: 10,
            color: false,
            background: None,
        };
        let expected = [
            "╭────────╮",
            "│        │",
            "│ ab     │",
            "│        │",
            "│ foot   │",
            "╰────────╯",
        ];
        let out = style.boxed(" ab", " foot", 6);
        assert_eq!(out.lines().collect::<Vec<_>>(), expected);
    }

    #[test]
    fn border_keeps_its_bottom_edge_when_the_body_is_too_tall() {
        let style = Style {
            width: 6,
            color: false,
            background: None,
        };
        let out = style.boxed("1\n2\n3\n4", "f", 5);
        let lines: Vec<_> = out.lines().collect();
        assert_eq!(lines, ["╭────╮", "│    │", "│1   │", "│f   │", "╰────╯"]);
    }

    #[test]
    fn no_background_without_color() {
        let style = Style {
            width: 6,
            color: false,
            background: Some("40".into()),
        };
        assert_eq!(style.fill("ab", 3), "ab");
    }

    #[test]
    fn parses_backgrounds() {
        assert_eq!(parse_background("black"), Ok(Some("40".into())));
        assert_eq!(parse_background("none"), Ok(None));
        assert_eq!(
            parse_background("#1A2b3c"),
            Ok(Some("48;2;26;43;60".into()))
        );
        for bad in ["", "#123", "1a2b3c", "#gggggg", "red"] {
            assert!(parse_background(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn says_when_nothing_is_installed() {
        let out = render(
            &[],
            now(),
            &Style {
                width: 60,
                color: false,
                background: None,
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
