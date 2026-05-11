extern crate alloc;

use alloc::string::String as AllocString;
use core::net::Ipv4Addr;

use defmt::{info, warn};
use embassy_net::icmp::PacketMetadata;
use embassy_net::icmp::ping::{PingError, PingManager, PingParams};
use embassy_net::{Config, Runner, Stack, StackResources};
use embassy_time::{Duration, with_timeout};
use esp_radio::wifi::{
    AccessPointInfo, AuthMethod, ClientConfig, ModeConfig, ScanConfig, WifiController, WifiDevice,
    WifiError,
};
use static_cell::StaticCell;

use crate::wifi::storage;
use crate::wifi::{
    PingResult, ScanAuth, WIFI_CMD_CHANNEL, WIFI_PASS_MAX_LEN, WIFI_PING_RESULT,
    WIFI_SCAN_MAX_RESULTS, WIFI_SCAN_RESULTS, WIFI_SSID_MAX_LEN, WIFI_STATUS, WIFI_STATUS_SIGNAL,
    WIFI_TASK_READY, WifiCommand, WifiScanResult, WifiState, WifiStatus,
};

const WIFI_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const WIFI_PING_TIMEOUT: Duration = Duration::from_secs(2);

static STACK_RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();

pub fn init_wifi(
    radio: &'static esp_radio::Controller<'static>,
    wifi_peripheral: esp_hal::peripherals::WIFI<'static>,
) -> Result<
    (
        WifiController<'static>,
        Stack<'static>,
        Runner<'static, WifiDevice<'static>>,
    ),
    WifiError,
> {
    let (controller, interfaces) =
        esp_radio::wifi::new(radio, wifi_peripheral, Default::default())?;
    let config = Config::dhcpv4(Default::default());
    let resources = STACK_RESOURCES.init(StackResources::new());
    let (stack, runner) = embassy_net::new(interfaces.sta, config, resources, 0xD15C_A11E_u64);
    Ok((controller, stack, runner))
}

#[embassy_executor::task]
pub async fn wifi_net_task(mut runner: Runner<'static, WifiDevice<'static>>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
pub async fn wifi_control_task(
    mut controller: WifiController<'static>,
    stack: Stack<'static>,
) -> ! {
    WIFI_TASK_READY.store(true, portable_atomic::Ordering::Relaxed);

    set_status(WifiStatus::default()).await;

    let mut last_credentials: Option<(
        heapless::String<WIFI_SSID_MAX_LEN>,
        heapless::String<WIFI_PASS_MAX_LEN>,
    )> = None;

    loop {
        match WIFI_CMD_CHANNEL.receive().await {
            WifiCommand::SaveDefault { ssid, passphrase } => {
                match storage::save_credentials(ssid.as_str(), passphrase.as_str()).await {
                    Ok(()) => {
                        info!("Saved default Wi-Fi credentials");
                    }
                    Err(_) => {
                        warn!("Failed to save default Wi-Fi credentials");
                        set_error("save failed").await;
                    }
                }
            }
            WifiCommand::ConnectSaved => match storage::load_credentials().await {
                Ok(Some((ssid, passphrase))) => {
                    last_credentials = Some((ssid.clone(), passphrase.clone()));
                    connect_with_credentials(&mut controller, stack, ssid, passphrase).await;
                }
                Ok(None) => {
                    set_error("no saved credentials").await;
                }
                Err(_) => {
                    set_error("credential load failed").await;
                }
            },
            WifiCommand::Connect { ssid, passphrase } => {
                last_credentials = Some((ssid.clone(), passphrase.clone()));
                connect_with_credentials(&mut controller, stack, ssid, passphrase).await;
            }
            WifiCommand::Reconnect => {
                if let Some((ssid, passphrase)) = last_credentials.clone() {
                    connect_with_credentials(&mut controller, stack, ssid, passphrase).await;
                } else {
                    match storage::load_credentials().await {
                        Ok(Some((ssid, passphrase))) => {
                            last_credentials = Some((ssid.clone(), passphrase.clone()));
                            connect_with_credentials(&mut controller, stack, ssid, passphrase)
                                .await;
                        }
                        Ok(None) => {
                            set_error("no credentials available").await;
                        }
                        Err(_) => {
                            set_error("credential load failed").await;
                        }
                    }
                }
            }
            WifiCommand::Disconnect => {
                disconnect_if_needed(&mut controller).await;
                set_status(WifiStatus::default()).await;
            }
            WifiCommand::Scan => {
                let status_before_scan = WIFI_STATUS.lock().await.clone();
                set_transient_state(WifiState::Scanning).await;
                if let Err(err) = ensure_started_sta(&mut controller).await {
                    warn!("Wi-Fi start failed for scan: {:?}", err);
                    set_error("wifi start failed").await;
                    continue;
                }

                let scan_config = ScanConfig::default().with_max(WIFI_SCAN_MAX_RESULTS);
                match controller.scan_with_config_async(scan_config).await {
                    Ok(results) => {
                        update_scan_results(results).await;
                        let mut restored = status_before_scan;
                        restored.last_error = None;

                        if stack.is_config_up() {
                            let (station_ip, gateway_ip) = ipv4_addrs_from_stack(stack);
                            restored.state = WifiState::Connected;
                            restored.station_ip = station_ip;
                            restored.gateway_ip = gateway_ip;
                        } else if restored.state == WifiState::Scanning {
                            restored.state = WifiState::Disconnected;
                            restored.station_ip = None;
                            restored.gateway_ip = None;
                        }

                        set_status(restored).await;
                    }
                    Err(err) => {
                        warn!("Wi-Fi scan failed: {:?}", err);
                        set_error("scan failed").await;
                    }
                }
            }
            WifiCommand::Ping { ip } => {
                if !stack.is_config_up() {
                    WIFI_PING_RESULT.signal(PingResult::NoLink);
                    continue;
                }

                let mut rx_buffer = [0u8; 128];
                let mut tx_buffer = [0u8; 128];
                let mut rx_meta = [PacketMetadata::EMPTY];
                let mut tx_meta = [PacketMetadata::EMPTY];
                let mut ping = PingManager::new(
                    stack,
                    &mut rx_meta,
                    &mut rx_buffer,
                    &mut tx_meta,
                    &mut tx_buffer,
                );
                let mut params = PingParams::new(ip);
                params
                    .set_count(1)
                    .set_timeout(WIFI_PING_TIMEOUT)
                    .set_rate_limit(Duration::from_millis(100));

                let result = match ping.ping(&params).await {
                    Ok(duration) => {
                        let ms = duration.as_millis();
                        let time_ms = if ms > u32::MAX as u64 {
                            u32::MAX
                        } else {
                            ms as u32
                        };
                        PingResult::Reply { time_ms }
                    }
                    Err(PingError::DestinationHostUnreachable) => PingResult::Timeout,
                    Err(PingError::InvalidTargetAddress)
                    | Err(PingError::SocketBindError(_))
                    | Err(PingError::SocketSendError(_))
                    | Err(PingError::SocketRecvError(_))
                    | Err(PingError::SocketSendTimeout) => PingResult::Error,
                };
                WIFI_PING_RESULT.signal(result);
            }
        }
    }
}

async fn ensure_started_sta(controller: &mut WifiController<'static>) -> Result<(), WifiError> {
    if !controller.is_started()? {
        let mode = ModeConfig::Client(ClientConfig::default());
        controller.set_config(&mode)?;
        controller.start_async().await?;
    }
    Ok(())
}

async fn disconnect_if_needed(controller: &mut WifiController<'static>) {
    match controller.is_connected() {
        Ok(true) => {
            if let Err(err) = controller.disconnect_async().await {
                warn!("disconnect_async failed: {:?}", err);
            }
        }
        Ok(false) => {}
        Err(err) => {
            warn!("is_connected failed before disconnect: {:?}", err);
        }
    }
}

async fn connect_with_credentials(
    controller: &mut WifiController<'static>,
    stack: Stack<'static>,
    ssid: heapless::String<WIFI_SSID_MAX_LEN>,
    passphrase: heapless::String<WIFI_PASS_MAX_LEN>,
) {
    set_status(WifiStatus {
        state: WifiState::Connecting,
        ssid: Some(ssid.clone()),
        station_ip: None,
        gateway_ip: None,
        last_error: None,
    })
    .await;

    let auth_methods = auth_methods_for_passphrase(passphrase.as_str());
    let mut saw_connect_attempt = false;

    for auth_method in auth_methods {
        let mode = ModeConfig::Client(
            ClientConfig::default()
                .with_ssid(AllocString::from(ssid.as_str()))
                .with_password(AllocString::from(passphrase.as_str()))
                .with_auth_method(*auth_method),
        );

        match controller.is_started() {
            Ok(false) => {
                if let Err(err) = controller.set_config(&mode) {
                    warn!("set_config failed before start: {:?}", err);
                    continue;
                }

                if let Err(err) = controller.start_async().await {
                    warn!("Wi-Fi start failed: {:?}", err);
                    set_error("wifi start failed").await;
                    return;
                }
            }
            Ok(true) => {
                disconnect_if_needed(controller).await;
                if let Err(err) = controller.set_config(&mode) {
                    warn!("set_config failed: {:?}", err);
                    continue;
                }
            }
            Err(err) => {
                warn!("is_started failed: {:?}", err);
                set_error("wifi start failed").await;
                return;
            }
        }

        saw_connect_attempt = true;
        if let Err(err) = controller.connect_async().await {
            warn!("connect_async failed with {:?}: {:?}", auth_method, err);
            continue;
        }

        if with_timeout(WIFI_CONNECT_TIMEOUT, stack.wait_config_up())
            .await
            .is_err()
        {
            warn!("DHCP timeout with {:?}", auth_method);
            disconnect_if_needed(controller).await;
            continue;
        }

        let (station_ip, gateway_ip) = ipv4_addrs_from_stack(stack);
        set_status(WifiStatus {
            state: WifiState::Connected,
            ssid: Some(ssid),
            station_ip,
            gateway_ip,
            last_error: None,
        })
        .await;
        return;
    }

    if saw_connect_attempt {
        set_error("connect failed").await;
    } else {
        set_error("config failed").await;
    }
}

fn auth_methods_for_passphrase(passphrase: &str) -> &'static [AuthMethod] {
    if passphrase.is_empty() {
        &[AuthMethod::None]
    } else {
        &[
            AuthMethod::Wpa3Personal,
            AuthMethod::Wpa2Wpa3Personal,
            AuthMethod::Wpa2Personal,
            AuthMethod::WpaWpa2Personal,
        ]
    }
}

fn ipv4_addrs_from_stack(stack: Stack<'static>) -> (Option<Ipv4Addr>, Option<Ipv4Addr>) {
    if let Some(config) = stack.config_v4() {
        let station = to_core_ipv4(config.address.address());
        let gateway = config.gateway.map(to_core_ipv4);
        (Some(station), gateway)
    } else {
        (None, None)
    }
}

fn to_core_ipv4(addr: embassy_net::Ipv4Address) -> Ipv4Addr {
    let octets = addr.octets();
    Ipv4Addr::new(octets[0], octets[1], octets[2], octets[3])
}

async fn set_transient_state(state: WifiState) {
    let current = WIFI_STATUS.lock().await.clone();
    let mut status = current;
    status.state = state;
    status.last_error = None;
    set_status(status).await;
}

async fn set_error(msg: &'static str) {
    let current = WIFI_STATUS.lock().await.clone();
    let mut status = current;
    status.state = WifiState::Error;
    status.last_error = Some(msg);
    set_status(status).await;
}

async fn set_status(status: WifiStatus) {
    {
        let mut guard = WIFI_STATUS.lock().await;
        *guard = status.clone();
    }
    WIFI_STATUS_SIGNAL.signal(status);
}

fn map_auth(auth: Option<AuthMethod>) -> ScanAuth {
    match auth {
        Some(AuthMethod::None) => ScanAuth::Open,
        Some(AuthMethod::Wpa) => ScanAuth::Wpa,
        Some(AuthMethod::Wpa2Personal) | Some(AuthMethod::WpaWpa2Personal) => {
            ScanAuth::Wpa2Personal
        }
        Some(AuthMethod::Wpa3Personal) => ScanAuth::Wpa3Personal,
        Some(AuthMethod::Wpa2Wpa3Personal) => ScanAuth::Wpa2Wpa3Personal,
        _ => ScanAuth::Unknown,
    }
}

async fn update_scan_results(results: alloc::vec::Vec<AccessPointInfo>) {
    let mut out = crate::wifi::WifiScanResults::new();

    for ap in results {
        let mut ssid = heapless::String::<WIFI_SSID_MAX_LEN>::new();
        if ssid.push_str(ap.ssid.as_str()).is_err() {
            continue;
        }

        let entry = WifiScanResult {
            ssid,
            channel: ap.channel,
            signal_strength: ap.signal_strength,
            auth: map_auth(ap.auth_method),
        };

        if out.entries.push(entry).is_err() {
            break;
        }
    }

    let mut guard = WIFI_SCAN_RESULTS.lock().await;
    *guard = out;
}
