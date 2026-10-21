//! Key presses and clicks for watch mode and the bar, so Esc, or a click on
//! the bar's dismiss label, can close the pane.

use std::io::{IsTerminal, Write};
use std::time::Duration;

/// Something read from the terminal.
#[derive(Debug, PartialEq, Eq)]
pub enum Input {
    /// Esc or q.
    Quit,
    /// A left click, at a 1-based column and row.
    Click {
        column: u16,
        row: u16,
    },
    Other,
}

/// Turns off line buffering and echo on stdin while alive, so each key
/// press can be read as it happens. Ctrl-C still interrupts.
pub struct Keys {
    saved: libc::termios,
    mouse: bool,
}

impl Keys {
    /// `None` when stdin isn't a terminal.
    pub fn new() -> Option<Self> {
        if !std::io::stdin().is_terminal() {
            return None;
        }
        // SAFETY: tcgetattr fills in the zeroed struct; both calls only
        // touch stdin's terminal settings.
        unsafe {
            let mut settings: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut settings) != 0 {
                return None;
            }
            let saved = settings;
            settings.c_lflag &= !(libc::ICANON | libc::ECHO);
            settings.c_cc[libc::VMIN] = 1;
            settings.c_cc[libc::VTIME] = 0;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &settings) != 0 {
                return None;
            }
            Some(Keys {
                saved,
                mouse: false,
            })
        }
    }

    /// Asks the terminal to report clicks too, until dropped.
    pub fn with_clicks(mut self) -> Self {
        print!("\x1b[?1000h\x1b[?1006h");
        let _ = std::io::stdout().flush();
        self.mouse = true;
        self
    }

    /// Waits up to `timeout` for input. `None` if there was none.
    pub fn read(&self, timeout: Duration) -> Option<Input> {
        let mut stdin = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: poll reads one valid pollfd, and read writes at most
        // buf.len() bytes into buf.
        let read = unsafe {
            if libc::poll(&mut stdin, 1, millis) <= 0 {
                return None;
            }
            let mut buf = [0u8; 32];
            let n = libc::read(libc::STDIN_FILENO, buf.as_mut_ptr().cast(), buf.len());
            if n <= 0 {
                return None;
            }
            buf[..n as usize].to_vec()
        };
        Some(parse(&read))
    }

    /// Waits up to `timeout` for input, and says whether it was Esc or q.
    pub fn quit_pressed(&self, timeout: Duration) -> bool {
        self.read(timeout) == Some(Input::Quit)
    }
}

impl Drop for Keys {
    fn drop(&mut self) {
        if self.mouse {
            print!("\x1b[?1006l\x1b[?1000l");
            let _ = std::io::stdout().flush();
        }
        // SAFETY: restores the settings read in `new`.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.saved);
        }
    }
}

/// Esc alone is Esc: an arrow key also starts with Esc, but arrives with
/// more bytes in the same read. Clicks come as SGR mouse reports, such as
/// `ESC [ < 0 ; 12 ; 1 M` for a left-button press at column 12, row 1.
fn parse(read: &[u8]) -> Input {
    if matches!(read, [0x1b] | [b'q']) {
        return Input::Quit;
    }
    let click = read
        .strip_prefix(b"\x1b[<")
        .and_then(|rest| rest.strip_suffix(b"M"))
        .and_then(|report| std::str::from_utf8(report).ok())
        .and_then(|report| {
            let mut fields = report.split(';').map(|f| f.parse::<u16>().ok());
            match (fields.next()??, fields.next()??, fields.next()??) {
                (0, column, row) => Some(Input::Click { column, row }),
                _ => None,
            }
        });
    click.unwrap_or(Input::Other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_and_q_quit_but_arrow_keys_do_not() {
        assert_eq!(parse(b"\x1b"), Input::Quit);
        assert_eq!(parse(b"q"), Input::Quit);
        assert_eq!(parse(b"\x1b[A"), Input::Other);
        assert_eq!(parse(b"x"), Input::Other);
        assert_eq!(parse(b""), Input::Other);
    }

    #[test]
    fn reads_left_clicks_only() {
        assert_eq!(parse(b"\x1b[<0;12;1M"), Input::Click { column: 12, row: 1 });
        // Release, right button, and a malformed report.
        assert_eq!(parse(b"\x1b[<0;12;1m"), Input::Other);
        assert_eq!(parse(b"\x1b[<2;12;1M"), Input::Other);
        assert_eq!(parse(b"\x1b[<0;x;1M"), Input::Other);
    }
}
