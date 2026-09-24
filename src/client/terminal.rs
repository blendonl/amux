use std::io::{self, Read, Write};
use std::thread;

use anyhow::Result;
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use tokio::sync::mpsc;

use crate::protocol::Size;

const STDIN_BUFFER_LEN: usize = 4096;
pub const STATUS_ROWS: u16 = 1;
const STDIN_CAPACITY: usize = 64;
const RESET_INPUT_MODES: &[u8] =
    b"\x1b[?1l\x1b>\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?25h\x1b[0m";

pub fn size() -> Result<Size> {
    let (cols, rows) = terminal::size()?;
    Ok(Size { rows, cols }.clamped())
}

pub fn session_size() -> Result<Size> {
    Ok(session_area(size()?))
}

pub fn session_area(size: Size) -> Size {
    Size {
        rows: size.rows.saturating_sub(STATUS_ROWS),
        cols: size.cols,
    }
    .clamped()
}

pub struct RawTerminal;

impl RawTerminal {
    pub fn enter() -> Result<Self> {
        terminal::enable_raw_mode()?;
        let guard = Self;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(guard)
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = stdout.write_all(RESET_INPUT_MODES);
        let _ = execute!(stdout, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

pub fn write_output(bytes: &[u8]) -> Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(bytes)?;
    stdout.flush()?;
    Ok(())
}

pub fn stdin_chunks() -> mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel(STDIN_CAPACITY);
    thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut buffer = [0; STDIN_BUFFER_LEN];
        loop {
            match stdin.read(&mut buffer) {
                Ok(0) => break,
                Ok(len) => {
                    if sender.blocking_send(buffer[..len].to_vec()).is_err() {
                        break;
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    });
    receiver
}
