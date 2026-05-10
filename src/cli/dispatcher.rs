extern crate alloc;

use alloc::boxed::Box;
use async_trait::async_trait;
use core::fmt::Write as FmtWrite;
use embedded_io_async::Write as AsyncWrite;
use heapless::Vec;

pub const MAX_ARGS: usize = 8;

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
        let mut args: Vec<&str, MAX_ARGS> = Vec::new();

        for arg in line.split_whitespace() {
            if args.push(arg).is_err() {
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
