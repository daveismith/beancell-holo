use core::fmt::Write as FmtWrite;

use embassy_time::Timer;
use embedded_io_async::{Read as AsyncRead, Write as AsyncWrite};
use heapless::{String, Vec};

use crate::cli::dispatcher::CommandDispatcher;

const MAX_LINE_SIZE: usize = 96;
const HISTORY_SIZE_BYTES: usize = 256;
const HISTORY_MAX_ENTRIES: usize = 16;

struct History {
    entries: Vec<String<MAX_LINE_SIZE>, HISTORY_MAX_ENTRIES>,
    bytes_used: usize,
}

impl History {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            bytes_used: 0,
        }
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn get(&self, index: usize) -> Option<&str> {
        self.entries.get(index).map(|s| s.as_str())
    }

    fn push(&mut self, line: &str) {
        if line.is_empty() {
            return;
        }

        let mut item: String<MAX_LINE_SIZE> = String::new();
        if item.push_str(line).is_err() {
            return;
        }

        let needed = item.len() + 1;
        while self.bytes_used + needed > HISTORY_SIZE_BYTES && !self.entries.is_empty() {
            let removed = self.entries.remove(0);
            self.bytes_used = self.bytes_used.saturating_sub(removed.len() + 1);
        }

        if self.entries.push(item).is_ok() {
            self.bytes_used += needed;
        }
    }
}

async fn write_raw<IO>(io: &mut IO, bytes: &[u8])
where
    IO: AsyncWrite,
{
    let _ = embedded_io_async::Write::write_all(io, bytes).await;
}

async fn redraw_line<IO>(io: &mut IO, prompt: &str, line: &str, cursor: usize)
where
    IO: AsyncWrite,
{
    write_raw(io, b"\r").await;
    write_raw(io, prompt.as_bytes()).await;
    write_raw(io, line.as_bytes()).await;
    write_raw(io, b"\x1b[K").await;

    let tail = line.len().saturating_sub(cursor);
    if tail > 0 {
        let mut esc: String<16> = String::new();
        let _ = core::fmt::write(&mut esc, format_args!("\x1b[{}D", tail));
        write_raw(io, esc.as_bytes()).await;
    }

    let _ = io.flush().await;
}

async fn read_one_byte<IO>(io: &mut IO) -> Option<u8>
where
    IO: AsyncRead,
{
    let mut buf = [0u8; 1];
    loop {
        match io.read(&mut buf).await {
            Ok(0) => Timer::after_millis(1).await,
            Ok(_) => return Some(buf[0]),
            Err(_) => Timer::after_millis(1).await,
        }
    }
}

async fn readline_with_history<IO>(
    io: &mut IO,
    prompt: &str,
    history: &History,
    history_cursor: &mut Option<usize>,
) -> Option<String<MAX_LINE_SIZE>>
where
    IO: AsyncRead + AsyncWrite,
{
    let mut line: String<MAX_LINE_SIZE> = String::new();
    let mut cursor: usize = 0;

    *history_cursor = None;
    redraw_line(io, prompt, line.as_str(), cursor).await;

    loop {
        let byte = read_one_byte(io).await?;

        match byte {
            b'\r' | b'\n' => {
                write_raw(io, b"\r\n").await;
                return Some(line);
            }
            0x08 | 0x7f if cursor > 0 => {
                line.remove(cursor - 1);
                cursor -= 1;
                redraw_line(io, prompt, line.as_str(), cursor).await;
            }
            0x1b => {
                let Some(next) = read_one_byte(io).await else {
                    continue;
                };
                if next != b'[' {
                    continue;
                }
                let Some(code) = read_one_byte(io).await else {
                    continue;
                };

                match code {
                    b'A' => {
                        if history.len() == 0 {
                            continue;
                        }
                        let next_index = match *history_cursor {
                            None => history.len() - 1,
                            Some(i) if i > 0 => i - 1,
                            Some(i) => i,
                        };
                        *history_cursor = Some(next_index);
                        line.clear();
                        if let Some(entry) = history.get(next_index) {
                            let _ = line.push_str(entry);
                        }
                        cursor = line.len();
                        redraw_line(io, prompt, line.as_str(), cursor).await;
                    }
                    b'B' => {
                        if let Some(i) = *history_cursor {
                            if i + 1 < history.len() {
                                let next_index = i + 1;
                                *history_cursor = Some(next_index);
                                line.clear();
                                if let Some(entry) = history.get(next_index) {
                                    let _ = line.push_str(entry);
                                }
                            } else {
                                *history_cursor = None;
                                line.clear();
                            }
                            cursor = line.len();
                            redraw_line(io, prompt, line.as_str(), cursor).await;
                        }
                    }
                    b'C' if cursor < line.len() => {
                        cursor += 1;
                        redraw_line(io, prompt, line.as_str(), cursor).await;
                    }
                    b'D' if cursor > 0 => {
                        cursor -= 1;
                        redraw_line(io, prompt, line.as_str(), cursor).await;
                    }
                    b'3' => {
                        let Some(tilde) = read_one_byte(io).await else {
                            continue;
                        };
                        if tilde == b'~' && cursor < line.len() {
                            line.remove(cursor);
                            redraw_line(io, prompt, line.as_str(), cursor).await;
                        }
                    }
                    _ => {}
                }
            }
            b if (b.is_ascii_graphic() || b == b' ')
                && line.len() < MAX_LINE_SIZE - 1
                && line.insert(cursor, b as char).is_ok() =>
            {
                cursor += 1;
                redraw_line(io, prompt, line.as_str(), cursor).await;
            }
            _ => {}
        }
    }
}

pub async fn run_cli<IO>(dispatcher: &CommandDispatcher<'_, IO>, io: &mut IO, prompt: &str) -> !
where
    IO: AsyncRead + AsyncWrite + FmtWrite,
{
    let mut history = History::new();
    let mut history_cursor: Option<usize> = None;

    loop {
        if let Some(line) = readline_with_history(io, prompt, &history, &mut history_cursor).await {
            let command_line = line.trim();
            if command_line.is_empty() {
                continue;
            }

            history.push(command_line);
            dispatcher.dispatch(command_line, io).await;
            io.flush().await.ok();
        } else {
            Timer::after_millis(5).await;
        }
    }
}
