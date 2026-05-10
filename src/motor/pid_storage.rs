use core::ops::Range;

use defmt::{info, warn};
use embedded_storage::nor_flash::{NorFlash as SyncNorFlash, ReadNorFlash as SyncReadNorFlash};
use embedded_storage_async::nor_flash::{ErrorType, MultiwriteNorFlash, NorFlash, ReadNorFlash};
use esp_bootloader_esp_idf::partitions::{
    DataPartitionSubType, PARTITION_TABLE_MAX_LEN, PartitionEntry, PartitionType,
    read_partition_table,
};
use esp_storage::{FlashStorage, FlashStorageError};
use sequential_storage::cache::NoCache;
use sequential_storage::map;

const PID_KEY: u8 = 1;
const PID_RESERVED_SECTORS: u32 = 2;
const FLASH_SECTOR_BYTES: u32 = 4096;

/// PID controller gains
#[derive(Clone, Copy, Debug, defmt::Format)]
pub struct PidGains {
    pub kp: f32,
    pub ki: f32,
    pub kd: f32,
}

impl Default for PidGains {
    fn default() -> Self {
        Self {
            kp: 0.5,
            ki: 0.01,
            kd: 0.1,
        }
    }
}

impl PidGains {
    /// Serialize to 12-byte buffer (3 × f32 in big-endian)
    pub fn to_bytes(&self) -> [u8; 12] {
        let mut bytes = [0u8; 12];
        bytes[0..4].copy_from_slice(&self.kp.to_be_bytes());
        bytes[4..8].copy_from_slice(&self.ki.to_be_bytes());
        bytes[8..12].copy_from_slice(&self.kd.to_be_bytes());
        bytes
    }

    /// Deserialize from 12-byte buffer
    pub fn from_bytes(bytes: &[u8; 12]) -> Self {
        let kp = f32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let ki = f32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let kd = f32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        Self { kp, ki, kd }
    }
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

fn find_pid_partition<'a>(
    table: &'a esp_bootloader_esp_idf::partitions::PartitionTable<'a>,
) -> Option<PartitionEntry<'a>> {
    table
        .find_partition(PartitionType::Data(DataPartitionSubType::Nvs))
        .ok()
        .flatten()
}

fn pid_flash_window() -> Option<Range<u32>> {
    let mut flash = FlashStorage::new();
    let mut partition_table_buf = [0u8; PARTITION_TABLE_MAX_LEN];
    let partition_table = read_partition_table(&mut flash, &mut partition_table_buf).ok()?;
    let partition = find_pid_partition(&partition_table)?;

    let part_offset = partition.offset();
    let part_len = partition.len();
    let reserve = PID_RESERVED_SECTORS * FLASH_SECTOR_BYTES;
    if part_len < reserve {
        warn!("NVS partition too small for sequential PID storage");
        return None;
    }

    let start = part_offset + part_len - reserve;
    let end = part_offset + part_len;
    Some(start..end)
}

/// NVS storage wrapper for PID gains
/// Note: NVS initialization requires esp-idf-svc which is conditionally compiled
pub struct PidTuningStorage {
    _namespace: &'static str,
    _key: &'static str,
}

impl Default for PidTuningStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl PidTuningStorage {
    pub const fn new() -> Self {
        Self {
            _namespace: "pid",
            _key: "gains",
        }
    }

    /// Load PID gains from non-volatile flash.
    pub async fn load_from_nvs(&self) -> Option<PidGains> {
        let _ = (self._namespace, self._key);
        let range = pid_flash_window()?;
        let mut flash = AsyncPartitionFlash::new(range.clone());
        let mut cache = NoCache::new();
        let mut data_buffer = [0u8; 64];

        let stored = map::fetch_item::<u8, [f32; 3], _>(
            &mut flash,
            0..(range.end - range.start),
            &mut cache,
            &mut data_buffer,
            &PID_KEY,
        )
        .await
        .ok()??;

        let gains = PidGains {
            kp: stored[0],
            ki: stored[1],
            kd: stored[2],
        };
        info!(
            "Loaded PID gains from flash: Kp={}, Ki={}, Kd={}",
            gains.kp, gains.ki, gains.kd
        );
        Some(gains)
    }

    /// Save PID gains to non-volatile flash.
    pub async fn save_to_nvs(&self, gains: PidGains) -> Result<(), ()> {
        let _ = (self._namespace, self._key);
        let range = pid_flash_window().ok_or(())?;
        let mut flash = AsyncPartitionFlash::new(range.clone());
        let mut cache = NoCache::new();
        let mut data_buffer = [0u8; 64];

        map::store_item(
            &mut flash,
            0..(range.end - range.start),
            &mut cache,
            &mut data_buffer,
            &PID_KEY,
            &[gains.kp, gains.ki, gains.kd],
        )
        .await
        .map_err(|_| ())?;

        info!(
            "Saved PID gains to flash: Kp={}, Ki={}, Kd={}",
            gains.kp, gains.ki, gains.kd
        );
        Ok(())
    }

    /// Clear PID gains from non-volatile flash.
    pub async fn clear_from_nvs(&self) -> Result<(), ()> {
        let _ = (self._namespace, self._key);
        let range = pid_flash_window().ok_or(())?;
        let mut flash = AsyncPartitionFlash::new(range.clone());
        let mut cache = NoCache::new();
        let mut data_buffer = [0u8; 64];

        map::remove_item::<u8, _>(
            &mut flash,
            0..(range.end - range.start),
            &mut cache,
            &mut data_buffer,
            &PID_KEY,
        )
        .await
        .map_err(|_| ())?;
        info!("Cleared PID gains from flash");
        Ok(())
    }
}
