# beancell-holo

## USB CLI

This firmware now includes a native USB CLI implementation under `src/cli`.
The `usb-cli` folder is treated as reference-only and is not required for build/runtime.

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

## Manual Smoke Test

Run the following in the CLI:

1. `help`
2. `echo hello from cli`
3. `reboot bootloader`
4. `reboot`

After `reboot`, reconnect with `tio /dev/tty.usbmodem101` and confirm prompt returns.

## Developer Notes

See `docs/usb-cli.md` for architecture and extension details.
