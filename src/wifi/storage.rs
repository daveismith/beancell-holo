use heapless::String;

use crate::shared_flash::{SharedFlash, SharedMapStorage};
use crate::wifi::{WIFI_PASS_MAX_LEN, WIFI_SSID_MAX_LEN};

const WIFI_SSID_KEY: u8 = 2;
const WIFI_PASS_KEY: u8 = 3;
const WIFI_RESERVED_SECTORS: u32 = 2;
const SSID_VALUE_LEN: usize = WIFI_SSID_MAX_LEN + 1;
const PASS_VALUE_LEN: usize = WIFI_PASS_MAX_LEN + 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageError {
    Flash,
    InvalidEncoding,
    ValueTooLong,
}

pub struct WifiCredentialStorage {
    map: SharedMapStorage,
}

impl WifiCredentialStorage {
    pub const fn new(flash: &'static SharedFlash) -> Self {
        Self {
            map: SharedMapStorage::new(flash, WIFI_RESERVED_SECTORS),
        }
    }

    pub async fn save_credentials(&self, ssid: &str, passphrase: &str) -> Result<(), StorageError> {
        let ssid_data = encode_len_prefixed::<SSID_VALUE_LEN>(ssid)?;
        let pass_data = encode_len_prefixed::<PASS_VALUE_LEN>(passphrase)?;

        self.map
            .store_value(WIFI_SSID_KEY, &ssid_data)
            .await
            .map_err(|_| StorageError::Flash)?;

        self.map
            .store_value(WIFI_PASS_KEY, &pass_data)
            .await
            .map_err(|_| StorageError::Flash)?;

        Ok(())
    }

    pub async fn load_credentials(
        &self,
    ) -> Result<Option<(String<WIFI_SSID_MAX_LEN>, String<WIFI_PASS_MAX_LEN>)>, StorageError> {
        let ssid_data = self
            .map
            .fetch_value::<[u8; SSID_VALUE_LEN]>(WIFI_SSID_KEY)
            .await
            .map_err(|_| StorageError::Flash)?;

        let pass_data = self
            .map
            .fetch_value::<[u8; PASS_VALUE_LEN]>(WIFI_PASS_KEY)
            .await
            .map_err(|_| StorageError::Flash)?;

        match (ssid_data, pass_data) {
            (Some(ssid_bytes), Some(pass_bytes)) => {
                let ssid = decode_len_prefixed::<WIFI_SSID_MAX_LEN, SSID_VALUE_LEN>(&ssid_bytes)?;
                let pass = decode_len_prefixed::<WIFI_PASS_MAX_LEN, PASS_VALUE_LEN>(&pass_bytes)?;
                Ok(Some((ssid, pass)))
            }
            _ => Ok(None),
        }
    }

    pub async fn clear_credentials(&self) -> Result<(), StorageError> {
        self.map
            .remove_key(WIFI_SSID_KEY)
            .await
            .map_err(|_| StorageError::Flash)?;

        self.map
            .remove_key(WIFI_PASS_KEY)
            .await
            .map_err(|_| StorageError::Flash)?;

        Ok(())
    }
}

fn encode_len_prefixed<const N: usize>(value: &str) -> Result<[u8; N], StorageError> {
    if value.len() > N.saturating_sub(1) {
        return Err(StorageError::ValueTooLong);
    }

    let mut out = [0u8; N];
    out[0] = value.len() as u8;
    out[1..(1 + value.len())].copy_from_slice(value.as_bytes());
    Ok(out)
}

fn decode_len_prefixed<const MAX: usize, const N: usize>(
    data: &[u8; N],
) -> Result<String<MAX>, StorageError> {
    let len = data[0] as usize;
    if len > MAX || len + 1 > N {
        return Err(StorageError::InvalidEncoding);
    }
    let utf8 =
        core::str::from_utf8(&data[1..(1 + len)]).map_err(|_| StorageError::InvalidEncoding)?;
    let mut out = String::<MAX>::new();
    out.push_str(utf8)
        .map_err(|_| StorageError::InvalidEncoding)?;
    Ok(out)
}
