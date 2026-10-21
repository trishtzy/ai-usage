//! Single key presses for watch mode, so Esc can close the pane.

use std::io::IsTerminal;
use std::time::Duration;

/// Turns off line buffering and echo on stdin while alive, so each key
/// press can be read as it happens. Ctrl-C still interrupts.
pub struct Keys {
    saved: libc::termios,
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
            Some(Keys { saved })
        }
    }

    /// Waits up to `timeout` for a key, and says whether it was one that
    /// closes the pane: Esc or q. An arrow key also starts with Esc, but
    /// arrives with more bytes in the same read.
    pub fn quit_pressed(&self, timeout: Duration) -> bool {
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
                return false;
            }
            let mut buf = [0u8; 16];
            let n = libc::read(libc::STDIN_FILENO, buf.as_mut_ptr().cast(), buf.len());
            if n <= 0 {
                return false;
            }
            buf[..n as usize].to_vec()
        };
        is_quit(&read)
    }
}

impl Drop for Keys {
    fn drop(&mut self) {
        // SAFETY: restores the settings read in `new`.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.saved);
        }
    }
}

fn is_quit(read: &[u8]) -> bool {
    matches!(read, [0x1b] | [b'q'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_and_q_quit_but_arrow_keys_do_not() {
        assert!(is_quit(b"\x1b"));
        assert!(is_quit(b"q"));
        assert!(!is_quit(b"\x1b[A"));
        assert!(!is_quit(b"x"));
        assert!(!is_quit(b""));
    }
}
