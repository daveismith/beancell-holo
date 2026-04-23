# USB CLI Architecture

## Goal

Provide a native, no-std CLI in the main firmware crate without compile/runtime dependency on the reference `usb-cli` folder.

## Module Layout

- `src/cli/dispatcher.rs`
  - `CommandHandler` async trait
  - `Command` metadata + boxed handler
  - `CommandDispatcher` with argument tokenization, built-in `help`, and command routing
- `src/cli/io.rs`
  - `CliIo` wrapper over `UsbSerialJtagRx/UsbSerialJtagTx`
  - Implements `embedded_io_async::Read`, `embedded_io_async::Write`, `core::fmt::Write`
- `src/cli/task.rs`
  - Prompt loop with built-in line editor
  - Supports cursor movement (left/right), backspace/delete, and command history (up/down)
  - Configured with fixed maximum line length and ~256-byte in-memory history budget
- `src/cli/handlers.rs`
  - `EchoCommand`
  - `RebootCommand` with subcommands `normal` and `bootloader`

## Wiring

Firmware wiring is in `src/bin/main.rs`:

1. Split USB Serial/JTAG into RX and TX.
2. Spawn `usb_cli_task` as Embassy task.
3. Register command table (`echo`, `reboot`).
4. Run CLI loop with prompt `beancell> `.

## Reboot Behavior

- `reboot` or `reboot normal` calls `esp_hal::system::software_reset()`.
- `reboot bootloader` prints an unsupported message for ESP32-C3 over this CLI transport.

## Add a New Command

1. Create a handler type implementing `CommandHandler<CliIo<'static>>` (or generic over IO traits).
2. Register it in the command table inside `usb_cli_task` in `src/bin/main.rs`.
3. Keep output deterministic to ease tests.

## Testing Strategy

- Firmware validation via `cargo build`.
- Device smoke testing via:
  - `cargo espflash flash`
  - `tio /dev/tty.usbmodem101`
- Verify editing/history behavior: left/right movement, backspace, up/down recall.

## Guardrail

The `usb-cli` directory is reference-only and can be deleted after implementation.
No app build dependency should point to it.
