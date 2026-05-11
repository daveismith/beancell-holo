extern crate alloc;

use alloc::boxed::Box;
use async_trait::async_trait;
use core::fmt::Write as FmtWrite;
use core::net::Ipv4Addr;
use core::str::FromStr;
use embedded_io_async::Write as AsyncWrite;

use crate::cli::CommandHandler;
use crate::wifi::{
    PingResult, WIFI_CMD_CHANNEL, WIFI_PASS_MAX_LEN, WIFI_PING_RESULT, WIFI_SCAN_RESULTS,
    WIFI_SSID_MAX_LEN, WIFI_STATUS, WIFI_TASK_READY, WifiCommand, WifiState,
};

pub struct WifiCommandHandler;

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for WifiCommandHandler
where
    IO: AsyncWrite + FmtWrite,
{
    async fn execute(&self, args: &[&str], io: &mut IO) {
        if args.len() < 2 {
            writeln!(
                io,
                "Usage: wifi <save|connect|connect-saved|reconnect|disconnect|status|ip|gateway|scan|scan-results|ping|clear>"
            )
            .ok();
            return;
        }

        if !WIFI_TASK_READY.load(portable_atomic::Ordering::Relaxed) {
            writeln!(io, "wifi: subsystem not initialized").ok();
            return;
        }

        match args[1] {
            "save" => {
                let (ssid, passphrase) = match parse_ssid_pass(args) {
                    Ok(pair) => pair,
                    Err(msg) => {
                        writeln!(io, "{}", msg).ok();
                        return;
                    }
                };

                WIFI_CMD_CHANNEL
                    .send(WifiCommand::SaveDefault { ssid, passphrase })
                    .await;
                writeln!(io, "wifi: saved default credentials").ok();
            }
            "connect-saved" => {
                WIFI_CMD_CHANNEL.send(WifiCommand::ConnectSaved).await;
                writeln!(io, "wifi: connect request sent").ok();
            }
            "connect" => {
                let (ssid, passphrase) = match parse_ssid_pass(args) {
                    Ok(pair) => pair,
                    Err(msg) => {
                        writeln!(io, "{}", msg).ok();
                        return;
                    }
                };

                WIFI_CMD_CHANNEL
                    .send(WifiCommand::Connect { ssid, passphrase })
                    .await;
                writeln!(io, "wifi: connect request sent").ok();
            }
            "reconnect" => {
                WIFI_CMD_CHANNEL.send(WifiCommand::Reconnect).await;
                writeln!(io, "wifi: reconnect request sent").ok();
            }
            "disconnect" => {
                WIFI_CMD_CHANNEL.send(WifiCommand::Disconnect).await;
                writeln!(io, "wifi: disconnect request sent").ok();
            }
            "status" => {
                let status = WIFI_STATUS.lock().await.clone();
                writeln!(io, "wifi state: {}", state_str(status.state)).ok();
                if let Some(ssid) = status.ssid {
                    writeln!(io, "ssid: {}", ssid).ok();
                }
                if let Some(ip) = status.station_ip {
                    writeln!(io, "station ip: {}", ip).ok();
                }
                if let Some(gw) = status.gateway_ip {
                    writeln!(io, "gateway ip: {}", gw).ok();
                }
                if let Some(err) = status.last_error {
                    writeln!(io, "last error: {}", err).ok();
                }
            }
            "ip" => {
                let status = WIFI_STATUS.lock().await.clone();
                match status.station_ip {
                    Some(ip) => writeln!(io, "{}", ip).ok(),
                    None => writeln!(io, "wifi: station ip unavailable").ok(),
                };
            }
            "gateway" => {
                let status = WIFI_STATUS.lock().await.clone();
                match status.gateway_ip {
                    Some(ip) => writeln!(io, "{}", ip).ok(),
                    None => writeln!(io, "wifi: gateway ip unavailable").ok(),
                };
            }
            "scan" => {
                WIFI_CMD_CHANNEL.send(WifiCommand::Scan).await;
                writeln!(io, "wifi: scan started").ok();
            }
            "scan-results" => {
                let scan = WIFI_SCAN_RESULTS.lock().await.clone();
                if scan.entries.is_empty() {
                    writeln!(io, "wifi: no scan results").ok();
                    return;
                }

                for entry in &scan.entries {
                    writeln!(
                        io,
                        "ssid={} rssi={} ch={} auth={}",
                        entry.ssid,
                        entry.signal_strength,
                        entry.channel,
                        auth_str(entry.auth)
                    )
                    .ok();
                }
            }
            "ping" => {
                let Some(raw_ip) = args.get(2) else {
                    writeln!(io, "Usage: wifi ping <ipv4>").ok();
                    return;
                };

                let ip = match Ipv4Addr::from_str(raw_ip) {
                    Ok(ip) => ip,
                    Err(_) => {
                        writeln!(io, "Invalid IPv4 address").ok();
                        return;
                    }
                };

                WIFI_CMD_CHANNEL.send(WifiCommand::Ping { ip }).await;
                match embassy_time::with_timeout(
                    embassy_time::Duration::from_secs(3),
                    WIFI_PING_RESULT.wait(),
                )
                .await
                {
                    Ok(PingResult::Reply { time_ms }) => {
                        writeln!(io, "reply from {}: {} ms", ip, time_ms).ok();
                    }
                    Ok(PingResult::Timeout) => {
                        writeln!(io, "ping timeout").ok();
                    }
                    Ok(PingResult::NoLink) => {
                        writeln!(io, "wifi link is down").ok();
                    }
                    Ok(PingResult::Error) => {
                        writeln!(io, "ping failed").ok();
                    }
                    Err(_) => {
                        writeln!(io, "ping timed out waiting for response").ok();
                    }
                }
            }
            "clear" => {
                match crate::wifi::storage::clear_credentials().await {
                    Ok(()) => writeln!(io, "wifi: cleared saved credentials").ok(),
                    Err(_) => writeln!(io, "wifi: failed to clear credentials").ok(),
                };
            }
            _ => {
                writeln!(
                    io,
                    "Usage: wifi <save|connect|connect-saved|reconnect|disconnect|status|ip|gateway|scan|scan-results|ping|clear>"
                )
                .ok();
            }
        }
    }
}

fn parse_ssid_pass(
    args: &[&str],
) -> Result<
    (
        heapless::String<WIFI_SSID_MAX_LEN>,
        heapless::String<WIFI_PASS_MAX_LEN>,
    ),
    &'static str,
> {
    if args.len() != 4 {
        return Err("Usage: wifi <save|connect> <ssid> <passphrase>");
    }

    let mut ssid = heapless::String::<WIFI_SSID_MAX_LEN>::new();
    ssid.push_str(args[2])
        .map_err(|_| "SSID too long (max 32 bytes)")?;

    let mut passphrase = heapless::String::<WIFI_PASS_MAX_LEN>::new();
    passphrase
        .push_str(args[3])
        .map_err(|_| "Passphrase too long (max 64 bytes)")?;

    Ok((ssid, passphrase))
}

fn state_str(state: WifiState) -> &'static str {
    match state {
        WifiState::Disconnected => "disconnected",
        WifiState::Connecting => "connecting",
        WifiState::Connected => "connected",
        WifiState::Scanning => "scanning",
        WifiState::Error => "error",
    }
}

fn auth_str(auth: crate::wifi::ScanAuth) -> &'static str {
    match auth {
        crate::wifi::ScanAuth::Open => "open",
        crate::wifi::ScanAuth::Wpa => "wpa",
        crate::wifi::ScanAuth::Wpa2Personal => "wpa2",
        crate::wifi::ScanAuth::Wpa3Personal => "wpa3",
        crate::wifi::ScanAuth::Wpa2Wpa3Personal => "wpa2-wpa3",
        crate::wifi::ScanAuth::Unknown => "unknown",
    }
}
