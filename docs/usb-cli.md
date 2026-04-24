# USB CLI Architecture

## Goal

Provide native, no-std CLI sessions in the main firmware crate for USB Serial JTAG and UART0 without compile/runtime dependency on the removed reference `usb-cli` folder.

## Module Layout

- `src/cli/dispatcher.rs`
  - `CommandHandler` async trait
  - `Command` metadata + boxed handler
  - `CommandDispatcher` with argument tokenization, built-in `help`, and command routing
- `src/cli/io.rs`
  - `UsbCliIo` wrapper over `UsbSerialJtagRx/UsbSerialJtagTx`
  - `UartCliIo` wrapper over `UartRx/UartTx`
  - Both implement `embedded_io_async::Read`, `embedded_io_async::Write`, `core::fmt::Write`
- `src/cli/task.rs`
  - Prompt loop with built-in line editor
  - Supports cursor movement (left/right), backspace/delete, and command history (up/down)
  - Configured with fixed maximum line length and ~256-byte in-memory history budget
  - History is per-session; each transport task has independent history state
- `src/cli/handlers.rs`
  - `EchoCommand`
  - `RebootCommand` with subcommands `normal` and `bootloader`

## Wiring

Firmware wiring is in `src/bin/main.rs`:

1. Split USB Serial/JTAG into RX and TX.
2. Initialize UART0 with RX GPIO3 / TX GPIO4 at 115200 and split RX/TX.
3. Spawn `usb_cli_task` and `uart_cli_task` as concurrent Embassy tasks.
4. Register the same command table (`echo`, `reboot`) per session.
5. Run CLI loop with prompt `beancell> ` on both interfaces.

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
- Validate UART session via connected USB-UART adapter terminal at 115200.
- Verify editing/history behavior: left/right movement, backspace, up/down recall.
- Verify output and history isolation between USB and UART sessions.

## Guardrail

The `usb-cli` directory is reference-only and can be deleted after implementation.
No app build dependency should point to it.
