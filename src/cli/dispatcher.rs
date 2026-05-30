extern crate alloc;

use alloc::boxed::Box;
use async_trait::async_trait;
use core::fmt::Write as FmtWrite;
use embedded_io_async::Write as AsyncWrite;
use heapless::{String, Vec};

pub const MAX_ARGS: usize = 8;
const MAX_ARG_LEN: usize = 96;

#[async_trait(?Send)]
pub trait CommandHandler<IO>: Send + Sync {
    async fn execute(&self, args: &[&str], io: &mut IO);
}

pub struct Command<IO> {
    pub name: &'static str,
    pub description: &'static str,
    pub handler: Box<dyn CommandHandler<IO>>,
}

impl<IO> Command<IO> {
    pub fn new<H>(name: &'static str, description: &'static str, handler: H) -> Self
    where
        H: CommandHandler<IO> + 'static,
    {
        Self {
            name,
            description,
            handler: Box::new(handler),
        }
    }
}

pub struct CommandDispatcher<'a, IO> {
    commands: &'a [Command<IO>],
}

impl<'a, IO> CommandDispatcher<'a, IO>
where
    IO: AsyncWrite + FmtWrite,
{
    pub fn new(commands: &'a [Command<IO>]) -> Self {
        Self { commands }
    }

    pub async fn dispatch(&self, line: &str, io: &mut IO) {
        let parsed_args = match parse_args(line) {
            Ok(args) => args,
            Err(ParseError::TooManyArgs) => {
                writeln!(io, "Too many arguments (max {}).", MAX_ARGS).ok();
                return;
            }
            Err(ParseError::ArgTooLong) => {
                writeln!(io, "Argument too long (max {} chars).", MAX_ARG_LEN).ok();
                return;
            }
            Err(ParseError::UnterminatedQuote) => {
                writeln!(io, "Unterminated quoted string.").ok();
                return;
            }
        };

        let mut args: Vec<&str, MAX_ARGS> = Vec::new();
        for arg in &parsed_args {
            if args.push(arg.as_str()).is_err() {
                writeln!(io, "Too many arguments (max {}).", MAX_ARGS).ok();
                return;
            }
        }

        if args.is_empty() {
            writeln!(io, "No command entered.").ok();
            return;
        }

        if args[0] == "help" {
            writeln!(io, "Available commands:").ok();
            writeln!(io, "  {:<10} - Show this help output", "help").ok();
            for cmd in self.commands {
                writeln!(io, "  {:<10} - {}", cmd.name, cmd.description).ok();
            }
            io.flush().await.ok();
            return;
        }

        for cmd in self.commands {
            if cmd.name == args[0] {
                cmd.handler.execute(&args, io).await;
                return;
            }
        }

        writeln!(io, "Unknown command: '{}'", args[0]).ok();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParseError {
    TooManyArgs,
    ArgTooLong,
    UnterminatedQuote,
}

fn parse_args(line: &str) -> Result<Vec<String<MAX_ARG_LEN>, MAX_ARGS>, ParseError> {
    let mut args: Vec<String<MAX_ARG_LEN>, MAX_ARGS> = Vec::new();
    let mut current: String<MAX_ARG_LEN> = String::new();
    let mut in_quotes = false;
    let mut escaping = false;
    let mut token_started = false;

    for ch in line.chars() {
        if escaping {
            current.push(ch).map_err(|_| ParseError::ArgTooLong)?;
            escaping = false;
            token_started = true;
            continue;
        }

        match ch {
            '\\' => {
                escaping = true;
                token_started = true;
            }
            '"' => {
                in_quotes = !in_quotes;
                token_started = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if token_started {
                    args.push(current).map_err(|_| ParseError::TooManyArgs)?;
                    current = String::new();
                    token_started = false;
                }
            }
            _ => {
                current.push(ch).map_err(|_| ParseError::ArgTooLong)?;
                token_started = true;
            }
        }
    }

    if escaping {
        current.push('\\').map_err(|_| ParseError::ArgTooLong)?;
    }

    if in_quotes {
        return Err(ParseError::UnterminatedQuote);
    }

    if token_started {
        args.push(current).map_err(|_| ParseError::TooManyArgs)?;
    }

    Ok(args)
}
