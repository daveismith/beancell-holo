extern crate alloc;

use alloc::boxed::Box;
use async_trait::async_trait;
use core::fmt::Write as FmtWrite;
use embedded_io_async::Write as AsyncWrite;

use crate::cli::CommandHandler;

pub struct EchoCommand;

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for EchoCommand
where
    IO: AsyncWrite + FmtWrite,
{
    async fn execute(&self, args: &[&str], io: &mut IO) {
        if args.len() < 2 {
            writeln!(io, "Usage: echo <message>").ok();
            return;
        }

        let mut message = heapless::String::<128>::new();
        for (index, word) in args[1..].iter().enumerate() {
            if index > 0 && message.push(' ').is_err() {
                writeln!(io, "Message too long").ok();
                return;
            }

            if message.push_str(word).is_err() {
                writeln!(io, "Message too long").ok();
                return;
            }
        }

        writeln!(io, "{}", message).ok();
    }
}

pub struct RebootCommand;

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for RebootCommand
where
    IO: AsyncWrite + FmtWrite,
{
    async fn execute(&self, args: &[&str], io: &mut IO) {
        let mode = args.get(1).copied().unwrap_or("normal");

        match mode {
            "normal" => {
                writeln!(io, "Rebooting now...").ok();
                io.flush().await.ok();
                embassy_time::Timer::after_millis(50).await;
                esp_hal::system::software_reset();
            }
            "bootloader" => {
                writeln!(
                    io,
                    "Bootloader mode is not supported via USB Serial/JTAG CLI on ESP32-C3."
                )
                .ok();
                writeln!(io, "Use `cargo espflash` for flashing workflows.").ok();
            }
            _ => {
                writeln!(io, "Usage: reboot [normal|bootloader]").ok();
            }
        }
    }
}
