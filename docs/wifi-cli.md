# Wi-Fi CLI

The firmware exposes a `wifi` command that wraps the Wi-Fi subsystem APIs.

## Commands

- `wifi save <ssid> [passphrase]`
  - Saves default credentials in the NVS partition used by sequential-storage.
  - Omit `passphrase` for open networks.
- `wifi connect <ssid> [passphrase]`
  - Starts a background connect flow.
  - Omit `passphrase` for open networks.
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

## Connection Architecture

The Wi-Fi control subsystem implements a clean, simplified direct connection flow:
1. When a connection is requested, the subsystem queries the in-memory scan results (populated via `wifi scan`) for the target SSID's authentication mode.
2. It maps the SSID and passphrase directly to the single most appropriate `AuthenticationMethod` (e.g., WPA3-Personal, WPA2-Personal, open, etc.).
3. It configures the station mode interface, immediately triggers `connect_async`, and awaits DHCP configuration.
4. It avoids complex and slow workarounds such as pre-connect active scanning, BSSID pinning, or nested multi-authentication auto-fallback. This provides maximum speed, stability, and predictability on top of the underlying radio capabilities.

## Quoting and Escaping

- Arguments can be wrapped in `"` to include spaces.
- Use `\"` to include a literal quote character in an argument.
- Quotes are not part of the resulting value.

Examples:

- `wifi connect "joshua 5GHz_2G" "secret pass"`
- `wifi connect "Open Network"`
- `wifi save "My \"Quoted\" SSID" "pa\"ss"`
