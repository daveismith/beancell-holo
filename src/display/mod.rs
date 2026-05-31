use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use heapless::{String, Vec};

pub mod storage;
pub mod task;

pub const DISPLAY_NAME_MAX_LEN: usize = 32;
pub const DISPLAY_PASS_MAX_LEN: usize = 64;
pub const DISPLAY_CMD_QUEUE_DEPTH: usize = 8;
pub const DISPLAY_MAX_FILES: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayPowerState {
    Off,
    PoweringOn,
    On,
    PoweringOff,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayPlayState {
    Playing,
    Paused,
    Offline,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigEntry {
    pub key: String<16>,
    pub value: String<64>,
}

#[derive(Clone, Debug)]
pub struct DisplayStatus {
    pub power: DisplayPowerState,
    pub wifi_connected: bool,
    pub current_file: Option<String<64>>,
    pub progress: u32,
    pub total: u32,
    pub play_state: DisplayPlayState,
    pub brightness: u8,
    pub volume: u8,
    pub loop_mode: Option<String<10>>,
    pub angle: Option<u16>,
    pub ble: Option<String<10>>,
    pub switcher: Option<String<10>>,
    pub ssid_config: Option<String<32>>,
    pub configs: Vec<ConfigEntry, 10>,
    pub files: Vec<String<64>, DISPLAY_MAX_FILES>,
    pub model: Option<String<16>>,
    pub sw_version: Option<String<16>>,
}

impl DisplayStatus {
    pub const fn new() -> Self {
        Self {
            power: DisplayPowerState::Off,
            wifi_connected: false,
            current_file: None,
            progress: 0,
            total: 0,
            play_state: DisplayPlayState::Offline,
            brightness: 3,
            volume: 3,
            loop_mode: None,
            angle: None,
            ble: None,
            switcher: None,
            ssid_config: None,
            configs: Vec::new(),
            files: Vec::new(),
            model: None,
            sw_version: None,
        }
    }
}

pub fn decode_hex_filename(hex: &str) -> Option<String<32>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let mut s = String::<32>::new();
    let bytes = hex.as_bytes();
    for i in (0..hex.len()).step_by(2) {
        let high = decode_hex_nibble(bytes[i])?;
        let low = decode_hex_nibble(bytes[i + 1])?;
        let byte = (high << 4) | low;
        if byte.is_ascii() {
            if s.push(byte as char).is_err() {
                return None;
            }
        } else {
            return None;
        }
    }
    Some(s)
}

pub fn encode_filename_to_hex(filename: &str) -> Option<String<64>> {
    let mut s = String::<64>::new();
    for &b in filename.as_bytes() {
        let high = b >> 4;
        let low = b & 0x0F;
        if s.push(encode_hex_nibble(high)).is_err() || s.push(encode_hex_nibble(low)).is_err() {
            return None;
        }
    }
    Some(s)
}

fn decode_hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn encode_hex_nibble(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'a' + (n - 10)) as char,
        _ => '0',
    }
}

impl Default for DisplayStatus {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug)]
pub enum DisplayCommand {
    PowerOn,
    PowerOff,
    SetWifi {
        ssid: String<DISPLAY_NAME_MAX_LEN>,
        passphrase: String<DISPLAY_PASS_MAX_LEN>,
    },
    PlayFile {
        filename_hex: String<64>,
    },
    Pause,
    Next,
    Previous,
    SetBrightness(u8),
    SetVolume(u8),
    SetLoop(String<10>),
    SetConfig {
        key: String<16>,
        value: String<48>,
    },
    GetDcim,
    GetConfig,
    GetStatus,
    SetSwitcher(bool),
    GetInfo,
}

pub static DISPLAY_CMD_CHANNEL: Channel<
    CriticalSectionRawMutex,
    DisplayCommand,
    DISPLAY_CMD_QUEUE_DEPTH,
> = Channel::new();

pub static DISPLAY_STATUS: Mutex<CriticalSectionRawMutex, DisplayStatus> =
    Mutex::new(DisplayStatus::new());
