use core::ops::Range;

use embedded_storage::nor_flash::{NorFlash as SyncNorFlash, ReadNorFlash as SyncReadNorFlash};
use embedded_storage_async::nor_flash::{ErrorType, MultiwriteNorFlash, NorFlash, ReadNorFlash};
use esp_bootloader_esp_idf::partitions::{
    DataPartitionSubType, PARTITION_TABLE_MAX_LEN, PartitionEntry, PartitionType,
    read_partition_table,
};
use esp_storage::{FlashStorage, FlashStorageError};
use heapless::String;
use sequential_storage::cache::NoCache;
use sequential_storage::map;

use crate::wifi::{WIFI_PASS_MAX_LEN, WIFI_SSID_MAX_LEN};

const WIFI_SSID_KEY: u8 = 2;
const WIFI_PASS_KEY: u8 = 3;
const WIFI_RESERVED_SECTORS: u32 = 2;
const FLASH_SECTOR_BYTES: u32 = 4096;
const SSID_VALUE_LEN: usize = WIFI_SSID_MAX_LEN + 1;
const PASS_VALUE_LEN: usize = WIFI_PASS_MAX_LEN + 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageError {
    Flash,
    InvalidEncoding,
    ValueTooLong,
}

struct AsyncPartitionFlash {
    flash: FlashStorage,
    abs_range: Range<u32>,
}

impl AsyncPartitionFlash {
    fn new(abs_range: Range<u32>) -> Self {
        Self {
            flash: FlashStorage::new(),
            abs_range,
        }
    }

    fn map_offset(&self, offset: u32, len: usize) -> Result<u32, FlashStorageError> {
        let end = offset.saturating_add(len as u32);
        let cap = self.capacity() as u32;
        if end > cap {
            return Err(FlashStorageError::OutOfBounds);
        }
        Ok(self.abs_range.start + offset)
    }
}

impl ErrorType for AsyncPartitionFlash {
    type Error = FlashStorageError;
}

impl ReadNorFlash for AsyncPartitionFlash {
    const READ_SIZE: usize = FlashStorage::WORD_SIZE as usize;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let abs = self.map_offset(offset, bytes.len())?;
        SyncReadNorFlash::read(&mut self.flash, abs, bytes)
    }

    fn capacity(&self) -> usize {
        (self.abs_range.end - self.abs_range.start) as usize
    }
}

impl NorFlash for AsyncPartitionFlash {
    const WRITE_SIZE: usize = FlashStorage::WORD_SIZE as usize;
    const ERASE_SIZE: usize = FlashStorage::SECTOR_SIZE as usize;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let len = to.saturating_sub(from) as usize;
        let abs_from = self.map_offset(from, 0)?;
        let abs_to = self.map_offset(to, 0)?;
        if len == 0 || to < from {
            return Err(FlashStorageError::OutOfBounds);
        }
        SyncNorFlash::erase(&mut self.flash, abs_from, abs_to)
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let abs = self.map_offset(offset, bytes.len())?;
        SyncNorFlash::write(&mut self.flash, abs, bytes)
    }
}

impl MultiwriteNorFlash for AsyncPartitionFlash {}

fn find_nvs_partition<'a>(
    table: &'a esp_bootloader_esp_idf::partitions::PartitionTable<'a>,
) -> Option<PartitionEntry<'a>> {
    table
        .find_partition(PartitionType::Data(DataPartitionSubType::Nvs))
        .ok()
        .flatten()
}

fn wifi_flash_window() -> Option<Range<u32>> {
    let mut flash = FlashStorage::new();
    let mut partition_table_buf = [0u8; PARTITION_TABLE_MAX_LEN];
    let partition_table = read_partition_table(&mut flash, &mut partition_table_buf).ok()?;
    let partition = find_nvs_partition(&partition_table)?;

    let part_offset = partition.offset();
    let part_len = partition.len();
    let reserve = WIFI_RESERVED_SECTORS * FLASH_SECTOR_BYTES;
    if part_len < reserve {
        return None;
    }

    let start = part_offset + part_len - reserve;
    let end = part_offset + part_len;
    Some(start..end)
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

pub async fn save_credentials(ssid: &str, passphrase: &str) -> Result<(), StorageError> {
    let range = wifi_flash_window().ok_or(StorageError::Flash)?;
    let mut flash = AsyncPartitionFlash::new(range.clone());
    let mut cache = NoCache::new();
    let mut data_buffer = [0u8; 96];

    let ssid_data = encode_len_prefixed::<SSID_VALUE_LEN>(ssid)?;
    let pass_data = encode_len_prefixed::<PASS_VALUE_LEN>(passphrase)?;

    map::store_item(
        &mut flash,
        0..(range.end - range.start),
        &mut cache,
        &mut data_buffer,
        &WIFI_SSID_KEY,
        &ssid_data,
    )
    .await
    .map_err(|_| StorageError::Flash)?;

    map::store_item(
        &mut flash,
        0..(range.end - range.start),
        &mut cache,
        &mut data_buffer,
        &WIFI_PASS_KEY,
        &pass_data,
    )
    .await
    .map_err(|_| StorageError::Flash)?;

    Ok(())
}

pub async fn load_credentials()
-> Result<Option<(String<WIFI_SSID_MAX_LEN>, String<WIFI_PASS_MAX_LEN>)>, StorageError> {
    let range = wifi_flash_window().ok_or(StorageError::Flash)?;
    let mut flash = AsyncPartitionFlash::new(range.clone());
    let mut cache = NoCache::new();
    let mut data_buffer = [0u8; 96];

    let ssid_data = map::fetch_item::<u8, [u8; SSID_VALUE_LEN], _>(
        &mut flash,
        0..(range.end - range.start),
        &mut cache,
        &mut data_buffer,
        &WIFI_SSID_KEY,
    )
    .await
    .map_err(|_| StorageError::Flash)?;

    let pass_data = map::fetch_item::<u8, [u8; PASS_VALUE_LEN], _>(
        &mut flash,
        0..(range.end - range.start),
        &mut cache,
        &mut data_buffer,
        &WIFI_PASS_KEY,
    )
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

pub async fn clear_credentials() -> Result<(), StorageError> {
    let range = wifi_flash_window().ok_or(StorageError::Flash)?;
    let mut flash = AsyncPartitionFlash::new(range.clone());
    let mut cache = NoCache::new();
    let mut data_buffer = [0u8; 96];

    map::remove_item::<u8, _>(
        &mut flash,
        0..(range.end - range.start),
        &mut cache,
        &mut data_buffer,
        &WIFI_SSID_KEY,
    )
    .await
    .map_err(|_| StorageError::Flash)?;

    map::remove_item::<u8, _>(
        &mut flash,
        0..(range.end - range.start),
        &mut cache,
        &mut data_buffer,
        &WIFI_PASS_KEY,
    )
    .await
    .map_err(|_| StorageError::Flash)?;

    Ok(())
}
