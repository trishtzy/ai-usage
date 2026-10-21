//! A one-line footer bar: every provider used today on a single line, which
//! scrolls sideways as a marquee when it is wider than the terminal.

use crate::status::mark;
use crate::usage::Card;

/// Columns between the end of the line and its start again, when scrolling.
const MARQUEE_GAP: usize = 8;
/// Width of each limit's bar, in cells.
const BAR: usize = 8;

/// One terminal cell: a character and the SGR parameters to draw it with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    ch: char,
    style: Option<&'static str>,
}

fn push(cells: &mut Vec<Cell>, text: &str, style: Option<&'static str>) {
    cells.extend(text.chars().map(|ch| Cell { ch, style }));
}

/// Lays out every card on one line, such as
/// `✻ Claude Code  Session ██▋░░░░░ 20% · Week ███▋░░░░ 46% · Today 1.2M tokens`.
pub fn line(cards: &[Card]) -> Vec<Cell> {
    let mut cells = Vec::new();
    if cards.is_empty() {
        push(&mut cells, "No AI usage today", Some("2"));
        return cells;
    }
    for (i, card) in cards.iter().enumerate() {
        if i > 0 {
            push(&mut cells, "   │   ", Some("90"));
        }
        let (glyph, color) = mark(card.provider);
        push(&mut cells, glyph, Some(color));
        push(&mut cells, " ", None);
        push(&mut cells, card.title, Some("1"));
        push(&mut cells, "  ", None);
        for (j, meter) in card.meters.iter().enumerate() {
            if j > 0 {
                push(&mut cells, " · ", Some("90"));
            }
            // A bar has little room, so shorten the usual window names.
            let label = match meter.label.as_str() {
                "Current session" => "Session",
                "This week" => "Week",
                "This month" => "Month",
                label => label,
            };
            push(&mut cells, label, Some("2"));
            push(&mut cells, " ", None);
            match meter.used_percent {
                Some(percent) => {
                    bar(&mut cells, percent);
                    push(&mut cells, &format!(" {percent:.0}%"), None);
                }
                None => push(&mut cells, &meter.value, None),
            }
        }
    }
    cells
}

/// A bar `BAR` cells wide, filled in eighths, coloured as in the status view.
fn bar(cells: &mut Vec<Cell>, percent: f64) {
    const PARTS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    let eighths = ((percent.clamp(0.0, 100.0) / 100.0) * (BAR * 8) as f64).round() as usize;
    let color = match percent {
        p if p >= 90.0 => "31",
        p if p >= 75.0 => "33",
        _ => "34",
    };
    for i in 0..BAR {
        let filled = eighths.saturating_sub(i * 8).min(8);
        let cell = match filled {
            8 => Cell {
                ch: '█',
                style: Some(color),
            },
            0 => Cell {
                ch: '░',
                style: Some("90"),
            },
            part => Cell {
                ch: PARTS[part],
                style: Some(color),
            },
        };
        cells.push(cell);
    }
}

/// Whether `cells` needs to scroll to fit in `width` columns, leaving a
/// column free at each end.
pub fn scrolls(cells: &[Cell], width: usize) -> bool {
    cells.len() + 2 > width
}

/// All `width` columns of the line, as text with escape sequences, on
/// `background` if given. A line that fits sits one column in; one that
/// doesn't is shown from `offset`, with the line repeating after a gap.
pub fn render(cells: &[Cell], width: usize, offset: usize, background: Option<&str>) -> String {
    let blank = Cell {
        ch: ' ',
        style: None,
    };
    let shown: Vec<Cell> = if scrolls(cells, width) {
        let looped: Vec<Cell> = cells
            .iter()
            .copied()
            .chain(std::iter::repeat_n(blank, MARQUEE_GAP))
            .collect();
        (0..width)
            .map(|i| looped[(offset + i) % looped.len()])
            .collect()
    } else {
        std::iter::once(blank)
            .chain(cells.iter().copied())
            .chain(std::iter::repeat(blank))
            .take(width)
            .collect()
    };
    // A reset clears the background too, so set it again after each one.
    let background = background.map_or(String::new(), |bg| format!("\x1b[{bg}m"));
    let mut out = background.clone();
    let mut current = None;
    for cell in shown {
        if cell.style != current {
            out.push_str("\x1b[0m");
            out.push_str(&background);
            if let Some(code) = cell.style {
                out.push_str(&format!("\x1b[{code}m"));
            }
            current = cell.style;
        }
        out.push(cell.ch);
    }
    out.push_str("\x1b[0m");
    out
}

/// A row of `width` blank columns on `background`.
pub fn blank_row(width: usize, background: Option<&str>) -> String {
    render(&[], width, 0, background)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::Meter;

    fn plain(cells: &[Cell]) -> String {
        cells.iter().map(|c| c.ch).collect()
    }

    fn strip(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    fn cards() -> Vec<Card> {
        vec![
            Card {
                provider: "claude",
                title: "Claude Code",
                as_of: None,
                meters: vec![
                    Meter::limit("Current session".into(), 50.0, None),
                    Meter::limit("This week".into(), 100.0, None),
                    Meter::today(1_200_000),
                ],
                notes: vec!["ignored in the bar".into()],
            },
            Card {
                provider: "opencode",
                title: "OpenCode",
                as_of: None,
                meters: vec![Meter::today(42)],
                notes: vec![],
            },
        ]
    }

    #[test]
    fn puts_every_provider_on_one_line() {
        assert_eq!(
            plain(&line(&cards())),
            "✻ Claude Code  Session ████░░░░ 50% · Week ████████ 100% · Today 1.2M tokens   │   ▣ OpenCode  Today 42 tokens"
        );
        assert_eq!(plain(&line(&[])), "No AI usage today");
    }

    #[test]
    fn a_line_that_fits_sits_one_column_in_and_fills_the_width() {
        let cells = line(&[]);
        assert!(!scrolls(&cells, 20));
        assert_eq!(strip(&render(&cells, 20, 7, None)), " No AI usage today  ");
    }

    #[test]
    fn a_long_line_scrolls_and_wraps_around() {
        let cells = line(&[]);
        assert!(scrolls(&cells, 10));
        assert_eq!(strip(&render(&cells, 10, 0, None)), "No AI usag");
        assert_eq!(strip(&render(&cells, 10, 12, None)), "today     ");
        // 17 characters, then an 8-column gap, then it starts again.
        assert_eq!(strip(&render(&cells, 10, 22, None)), "   No AI u");
        assert_eq!(
            strip(&render(&cells, 10, 25, None)),
            strip(&render(&cells, 10, 0, None))
        );
    }

    #[test]
    fn styles_change_only_between_runs() {
        let cells = line(&[]);
        assert_eq!(
            render(&cells, 19, 0, None),
            " \x1b[0m\x1b[2mNo AI usage today\x1b[0m \x1b[0m"
        );
    }

    #[test]
    fn the_background_comes_back_after_every_reset() {
        let cells = line(&[]);
        assert_eq!(
            render(&cells, 19, 0, Some("40")),
            "\x1b[40m \x1b[0m\x1b[40m\x1b[2mNo AI usage today\x1b[0m\x1b[40m \x1b[0m"
        );
        assert_eq!(blank_row(3, Some("40")), "\x1b[40m   \x1b[0m");
    }
}
