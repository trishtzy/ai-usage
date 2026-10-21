//! A one-line bar: every provider used today on a single line, which
//! scrolls sideways as a marquee.

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

/// Whether `cells` is too wide for `width` columns, leaving a column free
/// at each end.
pub fn overflows(cells: &[Cell], width: usize) -> bool {
    cells.len() + 2 > width
}

/// How many columns a scrolling line moves before it's back where it
/// started: the line and the gap after it.
pub fn period(cells: &[Cell], width: usize) -> usize {
    cells.len() + MARQUEE_GAP.max(width.saturating_sub(cells.len()))
}

/// All `width` columns of the line, as text with escape sequences, on
/// `background` if given. Still, the line sits one column in. Scrolling,
/// it's shown from `offset` and comes round again after a gap: just past
/// the edge for a line that overflows, or once it has crossed the whole
/// width for one that fits, like a ticker.
pub fn render(
    cells: &[Cell],
    width: usize,
    offset: usize,
    scroll: bool,
    background: Option<&str>,
) -> String {
    paint(&visible(cells, width, offset, scroll), background)
}

/// The fixed label at the bar's right, which closes it when clicked.
pub const DISMISS: &str = "dismiss (esc)";
/// Columns the label takes with the space around it: two before, one after.
const DISMISS_SPACE: usize = DISMISS.len() + 3;
/// The narrowest the scrolling line gets before the label is left out.
const MIN_LINE: usize = 20;

/// Columns left for the scrolling line beside the dismiss label.
pub fn line_width(width: usize) -> usize {
    if width >= DISMISS_SPACE + MIN_LINE {
        width - DISMISS_SPACE
    } else {
        width
    }
}

/// The 1-based columns the dismiss label takes up, if it's shown.
pub fn dismiss_columns(width: usize) -> Option<std::ops::RangeInclusive<usize>> {
    (line_width(width) < width).then(|| width - DISMISS.len()..=width - 1)
}

/// A whole bar row: the line scrolling in `line_width(width)` columns, then
/// the dismiss label fixed at the right.
pub fn render_bar(
    cells: &[Cell],
    width: usize,
    offset: usize,
    scroll: bool,
    background: Option<&str>,
) -> String {
    let line_width = line_width(width);
    let mut shown = visible(cells, line_width, offset, scroll);
    if line_width < width {
        push(&mut shown, "  ", None);
        push(&mut shown, DISMISS, Some("2"));
        push(&mut shown, " ", None);
    }
    paint(&shown, background)
}

/// The `width` cells of the line to show at `offset`.
fn visible(cells: &[Cell], width: usize, offset: usize, scroll: bool) -> Vec<Cell> {
    let blank = Cell {
        ch: ' ',
        style: None,
    };
    if scroll && !cells.is_empty() {
        let gap = period(cells, width) - cells.len();
        let looped: Vec<Cell> = cells
            .iter()
            .copied()
            .chain(std::iter::repeat_n(blank, gap))
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
    }
}

/// `cells` as text with escape sequences, on `background` if given.
fn paint(cells: &[Cell], background: Option<&str>) -> String {
    // A reset clears the background too, so set it again after each one.
    let background = background.map_or(String::new(), |bg| format!("\x1b[{bg}m"));
    let mut out = background.clone();
    let mut current = None;
    for &cell in cells {
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
    render(&[], width, 0, false, background)
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
    fn a_still_line_sits_one_column_in_and_fills_the_width() {
        let cells = line(&[]);
        assert!(!overflows(&cells, 20));
        assert_eq!(
            strip(&render(&cells, 20, 7, false, None)),
            " No AI usage today  "
        );
    }

    #[test]
    fn a_line_that_overflows_comes_round_just_past_the_edge() {
        let cells = line(&[]);
        assert!(overflows(&cells, 10));
        assert_eq!(strip(&render(&cells, 10, 0, true, None)), "No AI usag");
        assert_eq!(strip(&render(&cells, 10, 12, true, None)), "today     ");
        // 17 characters, then an 8-column gap, then it starts again.
        assert_eq!(strip(&render(&cells, 10, 22, true, None)), "   No AI u");
        assert_eq!(
            strip(&render(&cells, 10, 25, true, None)),
            strip(&render(&cells, 10, 0, true, None))
        );
    }

    #[test]
    fn a_line_that_fits_crosses_the_whole_width_like_a_ticker() {
        let cells = line(&[]);
        // 17 characters and a 13-column gap fill the 30 columns exactly.
        assert_eq!(
            strip(&render(&cells, 30, 0, true, None)),
            format!("No AI usage today{}", " ".repeat(13))
        );
        assert_eq!(
            strip(&render(&cells, 30, 6, true, None)),
            format!("usage today{}No AI ", " ".repeat(13))
        );
        assert_eq!(
            strip(&render(&cells, 30, 30, true, None)),
            strip(&render(&cells, 30, 0, true, None))
        );
    }

    #[test]
    fn pins_the_dismiss_label_at_the_right() {
        let cells = line(&[]);
        let width = DISMISS_SPACE + 25;
        let row = strip(&render_bar(&cells, width, 0, false, None));
        assert_eq!(
            row,
            format!(" No AI usage today{}  dismiss (esc) ", " ".repeat(7))
        );
        assert_eq!(row.chars().count(), width);
        // Scrolling moves the line but not the label.
        let row = strip(&render_bar(&cells, width, 3, true, None));
        assert!(row.starts_with("AI usage today"), "{row:?}");
        assert!(row.ends_with("  dismiss (esc) "), "{row:?}");

        // The label's 1-based columns, for clicks.
        let columns = dismiss_columns(width).unwrap();
        let label: String = row
            .chars()
            .skip(columns.start() - 1)
            .take(columns.clone().count())
            .collect();
        assert_eq!(label, DISMISS);
    }

    #[test]
    fn leaves_the_label_out_when_the_bar_is_narrow() {
        let width = DISMISS_SPACE + MIN_LINE - 1;
        assert_eq!(line_width(width), width);
        assert_eq!(dismiss_columns(width), None);
        let row = strip(&render_bar(&line(&[]), width, 0, false, None));
        assert!(!row.contains("dismiss"));
    }

    #[test]
    fn styles_change_only_between_runs() {
        let cells = line(&[]);
        assert_eq!(
            render(&cells, 19, 0, false, None),
            " \x1b[0m\x1b[2mNo AI usage today\x1b[0m \x1b[0m"
        );
    }

    #[test]
    fn the_background_comes_back_after_every_reset() {
        let cells = line(&[]);
        assert_eq!(
            render(&cells, 19, 0, false, Some("40")),
            "\x1b[40m \x1b[0m\x1b[40m\x1b[2mNo AI usage today\x1b[0m\x1b[40m \x1b[0m"
        );
        assert_eq!(blank_row(3, Some("40")), "\x1b[40m   \x1b[0m");
    }
}
