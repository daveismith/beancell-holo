# Spinning Display Integration

The spinning display integration provides high-performance, robust, and completely deadlock-free control over the spinning display platform using an ESP32-C3 microcontroller.

The active code logic driving this integration resides in [src/display/task.rs](src/display/task.rs), [src/display/mod.rs](src/display/mod.rs), and [src/cli/handlers.rs](src/cli/handlers.rs).

## Hardware Configuration

- **Power Control**: The display platform's physical power is controlled via a relay connected to general-purpose output pin `GPIO5`.
- **UART RX/TX**: High-speed, non-blocking serial communication for the CLI is kept on `GPIO3` (UART0 RX) and `GPIO4` (UART0 TX) to prevent hardware interface conflicts.

## CLI Commands Reference

All display actions are exposed through the `display` command line interface in [src/cli/handlers.rs](src/cli/handlers.rs).

- `display power on`
  - Activates the `GPIO5` relay to power up the display platform. High-performance AP discovery scans dynamically rather than waiting on static timers.
  - Automatically initializes Wi-Fi scanning to find and connect to the display AP (`5D_XXXX`) using the security password `12345678`.
- `display power off`
  - Sends a session quit command (`/ctrl/session?action=quit`) to the display platform.
  - Powers down the platform by releasing `GPIO5` to Low.
  - Clears WebSocket & TCP handle states.
- `display wifi <ssid> [passphrase]`
  - Manually configures the SSID and passphrase for connecting to the display platform. Saving credentials utilizes centralized non-volatile storage (NVS) keys 4 and 5 via sequential-storage, as defined in [src/display/storage.rs](src/display/storage.rs).
- `display play <filename>`
  - Sends a play command for a filename (such as `F_1389.bin`), which is automatically hex-encoded to the expected 20-character hexadecimal representation (`465f313338392e62696e`) before sending.
- `display pause`
  - Commands the display platform to pause video playback.
- `display next` / `display prev`
  - Traverses the video list forward or backward.
- `display brightness <1-3>`
  - Sets the LED panel brightness to level 1, 2, or 3.
- `display volume <1-3>`
  - Sets the audio output level of the display.
- `display loop <one|all>`
  - Adjusts video loop playback modes.
- `display mute` / `display unmute`
  - Shorthand commands to set the platform volume to 1 (mute) or 3 (unmute).
- `display dcim`
  - Retrieves catalog list of files from `/DCIM` and displays decoded human-readable filenames (e.g., `F_1389.bin`) next to their raw hex strings.
- `display config [<key> <value>]`
  - When run without arguments: Fetches and displays all active settings dynamically parsed from the JSON response, falling back to cached system metrics if unavailable.
  - When run with arguments: Issues a custom parameter set command back to the display (e.g. `/ctrl/set?<key>=<value>`).
- `display switcher <on|off>`
  - Explicitly enables or disables the display panel. If `switcher` is off, the display ceases playback and ignores subsequent play commands.
- `display info`
  - Queries the `/info` endpoint to fetch metadata of the physical display (e.g., active model code like `F-MINI12` and software version like `1.1.0`).
- `display status`
  - Displays the power state, station Wi-Fi status, play state (playing/paused/offline), current active filename (fully hex-decoded), and the numeric play progress vs total duration.

## System Architecture & Starvation Mitigation

The display control subsystem is structured specifically to eliminate interface hangs, priority starvation, and execution deadlocks:

1. **Dual-Concurrency Dual-Loop Split**:
   - The active tracking flow is divided into two separate asynchronous loops: the **Command Loop** and the **WebSocket Monitor Loop** (handled concurrently via structured `select` in [src/display/task.rs](src/display/task.rs)).
   - The **Command Loop** handles incoming client instructions from the CLI serial queue in real time without any possibility of being blocked by network connection latency.
   - The **WebSocket Monitor Loop** manages the port 9000 telemetry channel, and handles the 15-second heartbeat packet loop without blocking HTTP request execution.

2. **Network I/O Timeout Protection**:
   - All standard HTTP client operations inside `http_get` are protected by a rapid `1500ms` non-blocking timeout layer (`with_timeout`).
   - Standard TCP handshake connections on WebSocket Port 9000 are capped by a `2000ms` timeout layer to ensure high-priority commands execute with zero delay.

3. **Fast-Fail Synchronous Dispatch**:
   - Instead of using blocking asynchronous queues inside CLI input tasks, dispatching uses a lightweight, borrow-checker-safe, synchronous `try_send` closure.
   - If the queue is saturated, it immediately fails and reports a clear "system busy" warning back to the command terminal. This ensures that serial input readers never block.

4. **Dynamic AP Probing & Scanning**:
   - Replaced fixed boot-up timers (such as old `8000ms` delays) with a split second `500ms` settling period followed by target-aware active scans.
   - Discovers any `5D_` network on the fly if stored details do not exist, or queries specifically for the registered AP. The scan terminates immediately when the SSID is visible, initiating connection logic without unnecessary delay.

5. **Stateful Panic-Free WebSocket Initiations**:
   - WebSocket connection attempts utilize `Option<Instant>` initialized to `None` for tracking the absolute timestamp of the last connection attempt.
   - This eliminates sub-zero duration subtractions on instant timers during early booting, preventing cold boot `underflow` panics.

6. **Escape-Aware Zero-Allocation JSON Parser**:
   - Realized character-by-character scanning inside `parse_config_response` to parse escaped characters (e.g., `\"`) correctly inside JSON objects like `"value": "{\"\":\"\"}"`. This avoids premature value truncation, enabling perfect deserialization inside a `no_std` microcontroller environment.
