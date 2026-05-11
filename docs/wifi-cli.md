# Wi-Fi CLI

The firmware exposes a `wifi` command that wraps the Wi-Fi subsystem APIs.

## Commands

- `wifi save <ssid> <passphrase>`
  - Saves default credentials in the NVS partition used by sequential-storage.
- `wifi connect <ssid> <passphrase>`
  - Starts a background connect flow using WPA2/WPA3 personal auth mode.
- `wifi connect-saved`
  - Connects using saved default credentials.
- `wifi reconnect`
  - Reconnects using the last explicit credentials, or saved defaults when unavailable.
- `wifi disconnect`
  - Disconnects station mode.
- `wifi scan`
  - Starts a background Wi-Fi scan.
- `wifi scan-results`
  - Prints cached scan results from the most recent scan.
- `wifi status`
  - Prints state, SSID, station IP, gateway IP, and last error.
- `wifi ip`
  - Prints station IPv4 address if available.
- `wifi gateway`
  - Prints gateway IPv4 address if available.
- `wifi ping <ipv4>`
  - Sends a single ICMP echo with a fixed 2s timeout.
- `wifi clear`
  - Clears saved default credentials.

## Notes

- This implementation is IPv4-only and uses DHCPv4 for address assignment.
- `wifi connect*` and `wifi scan` are fire-and-forget by design.
- Use `wifi status` and `wifi scan-results` to inspect async results.
