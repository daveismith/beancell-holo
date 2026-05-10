# beancell-holo

## USB CLI

This firmware now includes native CLI implementations under `src/cli` on:

- USB Serial JTAG
- UART0 (RX GPIO3, TX GPIO4 @ 115200, no flow control)

Each interface runs its own CLI session task. Command output and history are scoped to the interface/session that executed the command.

### Available Commands

- `help` - List all commands
- `echo <message>` - Echo back the message
- `reboot [normal|bootloader]`
  - `normal` (default): perform software reset
  - `bootloader`: reports unsupported behavior for ESP32-C3 USB Serial/JTAG CLI path

### Line Editing and History

- Built-in line editing supports left/right arrow movement, backspace, and delete.
- Built-in history supports up/down recall.
- History is in-memory only, bounded to ~256 bytes, and resets on reboot.

## Build, Flash, and Connect

### Build

```bash
cargo build
```

### Flash

```bash
cargo espflash flash
```

### Connect to CLI

```bash
tio /dev/tty.usbmodem101
```

For UART, connect your USB-UART adapter at 115200 baud and open the corresponding `/dev/tty.*` device.

## Manual Smoke Test

Run the following in the CLI:

1. `help`
2. `echo hello from cli`
3. `reboot bootloader`
4. `reboot`

After `reboot`, reconnect with `tio /dev/tty.usbmodem101` and confirm prompt returns.

## Developer Notes

See `docs/usb-cli.md` for architecture and extension details.
