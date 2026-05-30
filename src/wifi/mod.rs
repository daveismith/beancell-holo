use core::net::Ipv4Addr;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use heapless::{String, Vec};
use portable_atomic::{AtomicBool, Ordering};

pub mod handlers;
pub mod storage;
pub mod task;

pub const WIFI_SSID_MAX_LEN: usize = 32;
pub const WIFI_PASS_MAX_LEN: usize = 64;
pub const WIFI_SCAN_MAX_RESULTS: usize = 16;
pub const WIFI_CMD_QUEUE_DEPTH: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiState {
    Disconnected,
    Connecting,
    Connected,
    Scanning,
    Error,
}

#[derive(Clone, Debug)]
pub struct WifiStatus {
    pub state: WifiState,
    pub ssid: Option<String<WIFI_SSID_MAX_LEN>>,
    pub station_ip: Option<Ipv4Addr>,
    pub gateway_ip: Option<Ipv4Addr>,
    pub last_error: Option<&'static str>,
}

impl Default for WifiStatus {
    fn default() -> Self {
        Self {
            state: WifiState::Disconnected,
            ssid: None,
            station_ip: None,
            gateway_ip: None,
            last_error: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanAuth {
    Open,
    Wpa,
    Wpa2Personal,
    Wpa3Personal,
    Wpa2Wpa3Personal,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct WifiScanResult {
    pub ssid: String<WIFI_SSID_MAX_LEN>,
    pub bssid: [u8; 6],
    pub channel: u8,
    pub signal_strength: i8,
    pub auth: ScanAuth,
}

#[derive(Clone, Debug)]
pub struct WifiScanResults {
    pub entries: Vec<WifiScanResult, WIFI_SCAN_MAX_RESULTS>,
}

impl WifiScanResults {
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl Default for WifiScanResults {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug)]
pub enum WifiCommand {
    SaveDefault {
        ssid: String<WIFI_SSID_MAX_LEN>,
        passphrase: String<WIFI_PASS_MAX_LEN>,
    },
    ClearCredentials,
    ConnectSaved,
    Connect {
        ssid: String<WIFI_SSID_MAX_LEN>,
        passphrase: String<WIFI_PASS_MAX_LEN>,
    },
    Reconnect,
    Disconnect,
    Scan,
    Ping {
        ip: Ipv4Addr,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PingResult {
    Reply { time_ms: u32 },
    Timeout,
    NoLink,
    Error,
}

pub static WIFI_CMD_CHANNEL: Channel<CriticalSectionRawMutex, WifiCommand, WIFI_CMD_QUEUE_DEPTH> =
    Channel::new();
pub static WIFI_STATUS_SIGNAL: Signal<CriticalSectionRawMutex, WifiStatus> = Signal::new();
pub static WIFI_STATUS: Mutex<CriticalSectionRawMutex, WifiStatus> = Mutex::new(WifiStatus {
    state: WifiState::Disconnected,
    ssid: None,
    station_ip: None,
    gateway_ip: None,
    last_error: None,
});
pub static WIFI_SCAN_RESULTS: Mutex<CriticalSectionRawMutex, WifiScanResults> =
    Mutex::new(WifiScanResults::new());
pub static WIFI_PING_RESULT: Signal<CriticalSectionRawMutex, PingResult> = Signal::new();
pub static WIFI_TASK_READY: AtomicBool = AtomicBool::new(false);

pub fn wifi_task_ready() -> bool {
    WIFI_TASK_READY.load(Ordering::Relaxed)
}
