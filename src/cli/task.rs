use core::fmt::Write as FmtWrite;

use embassy_time::Timer;
use embedded_io_async::{Read as AsyncRead, Write as AsyncWrite};
use heapless::String;

use crate::cli::dispatcher::CommandDispatcher;

const MAX_LINE_SIZE: usize = 96;

pub async fn run_cli<IO>(dispatcher: &CommandDispatcher<'_, IO>, io: &mut IO, prompt: &str) -> !
where
    IO: AsyncRead + AsyncWrite + FmtWrite,
{
    let mut line = String::<MAX_LINE_SIZE>::new();
    let mut read_buf = [0u8; 1];

    loop {
        write!(io, "{}", prompt).ok();
        io.flush().await.ok();
        line.clear();

        loop {
            match io.read(&mut read_buf).await {
                Ok(0) => {
                    Timer::after_millis(2).await;
                }
                Ok(_) => {
                    let byte = read_buf[0];
                    match byte {
                        b'\r' | b'\n' => {
                            writeln!(io).ok();
                            break;
                        }
                        0x08 | 0x7f => {
                            if !line.is_empty() {
                                line.pop();
                                write!(io, "\x08 \x08").ok();
                            }
                        }
                        0x03 => {
                            writeln!(io, "^C").ok();
                            line.clear();
                            break;
                        }
                        byte if byte.is_ascii_graphic() || byte == b' ' => {
                            if line.push(byte as char).is_ok() {
                                write!(io, "{}", byte as char).ok();
                            } else {
                                writeln!(io, "\r\nLine too long (max {}).", MAX_LINE_SIZE).ok();
                                line.clear();
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                Err(_) => {
                    Timer::after_millis(2).await;
                }
            }
        }

        let command_line = line.trim();
        if command_line.is_empty() {
            continue;
        }

        dispatcher.dispatch(command_line, io).await;
        io.flush().await.ok();
    }
}
