use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_storage::nor_flash::{NorFlash as SyncNorFlash, ReadNorFlash as SyncReadNorFlash};
use embedded_storage_async::nor_flash::{ErrorType, MultiwriteNorFlash, NorFlash, ReadNorFlash};
use esp_bootloader_esp_idf::partitions::{
    DataPartitionSubType, PARTITION_TABLE_MAX_LEN, PartitionEntry, PartitionType,
    read_partition_table,
};
use esp_storage::FlashStorage;
use sequential_storage::cache::NoCache;
use sequential_storage::map::{MapConfig, MapStorage, Value};
use static_cell::StaticCell;

use core::ops::Range;

pub type SharedFlash = Mutex<CriticalSectionRawMutex, FlashStorage<'static>>;

static FLASH_STORAGE: StaticCell<SharedFlash> = StaticCell::new();

const FLASH_SECTOR_BYTES: u32 = 4096;
const SHARED_MAP_BUFFER_LEN: usize = 128;

pub fn init_shared_flash(flash: esp_hal::peripherals::FLASH<'static>) -> &'static SharedFlash {
    FLASH_STORAGE.init(Mutex::new(FlashStorage::new(flash)))
}

struct AsyncPartitionFlash<'a> {
    flash: &'a mut FlashStorage<'static>,
    abs_range: Range<u32>,
}

impl AsyncPartitionFlash<'_> {
    fn map_offset(&self, offset: u32, len: usize) -> Result<u32, esp_storage::FlashStorageError> {
        let end = offset.saturating_add(len as u32);
        let cap = self.capacity() as u32;
        if end > cap {
            return Err(esp_storage::FlashStorageError::OutOfBounds);
        }
        Ok(self.abs_range.start + offset)
    }
}

impl ErrorType for AsyncPartitionFlash<'_> {
    type Error = esp_storage::FlashStorageError;
}

impl ReadNorFlash for AsyncPartitionFlash<'_> {
    const READ_SIZE: usize = FlashStorage::WORD_SIZE as usize;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let abs = self.map_offset(offset, bytes.len())?;
        SyncReadNorFlash::read(&mut self.flash, abs, bytes)
    }

    fn capacity(&self) -> usize {
        (self.abs_range.end - self.abs_range.start) as usize
    }
}

impl NorFlash for AsyncPartitionFlash<'_> {
    const WRITE_SIZE: usize = FlashStorage::WORD_SIZE as usize;
    const ERASE_SIZE: usize = FlashStorage::SECTOR_SIZE as usize;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let len = to.saturating_sub(from) as usize;
        let abs_from = self.map_offset(from, 0)?;
        let abs_to = self.map_offset(to, 0)?;
        if len == 0 || to < from {
            return Err(esp_storage::FlashStorageError::OutOfBounds);
        }
        SyncNorFlash::erase(&mut self.flash, abs_from, abs_to)
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let abs = self.map_offset(offset, bytes.len())?;
        SyncNorFlash::write(&mut self.flash, abs, bytes)
    }
}

impl MultiwriteNorFlash for AsyncPartitionFlash<'_> {}

fn find_nvs_partition<'a>(
    table: &'a esp_bootloader_esp_idf::partitions::PartitionTable<'a>,
) -> Option<PartitionEntry<'a>> {
    table
        .find_partition(PartitionType::Data(DataPartitionSubType::Nvs))
        .ok()
        .flatten()
}

fn nvs_tail_window(flash: &mut FlashStorage<'static>, reserved_sectors: u32) -> Option<Range<u32>> {
    let mut partition_table_buf = [0u8; PARTITION_TABLE_MAX_LEN];
    let partition_table = read_partition_table(flash, &mut partition_table_buf).ok()?;
    let partition = find_nvs_partition(&partition_table)?;

    let part_offset = partition.offset();
    let part_len = partition.len();
    let reserve = reserved_sectors * FLASH_SECTOR_BYTES;
    if part_len < reserve {
        return None;
    }

    let start = part_offset + part_len - reserve;
    let end = part_offset + part_len;
    Some(start..end)
}

pub struct SharedMapStorage {
    flash: &'static SharedFlash,
    reserved_sectors: u32,
}

impl SharedMapStorage {
    pub const fn new(flash: &'static SharedFlash, reserved_sectors: u32) -> Self {
        Self {
            flash,
            reserved_sectors,
        }
    }

    pub async fn fetch_value<V>(&self, key: u8) -> Result<Option<V>, ()>
    where
        for<'d> V: Value<'d>,
    {
        let mut flash = self.flash.lock().await;
        let abs_range = nvs_tail_window(&mut flash, self.reserved_sectors).ok_or(())?;
        let flash_range = 0..(abs_range.end - abs_range.start);
        let flash = AsyncPartitionFlash {
            flash: &mut flash,
            abs_range,
        };
        let config = MapConfig::new(flash_range);
        let mut storage = MapStorage::new(flash, config, NoCache::new());
        let mut data_buffer = [0u8; SHARED_MAP_BUFFER_LEN];
        storage
            .fetch_item::<V>(&mut data_buffer, &key)
            .await
            .map_err(|_| ())
    }

    pub async fn store_value<V>(&self, key: u8, value: &V) -> Result<(), ()>
    where
        for<'d> V: Value<'d>,
    {
        let mut flash = self.flash.lock().await;
        let abs_range = nvs_tail_window(&mut flash, self.reserved_sectors).ok_or(())?;
        let flash_range = 0..(abs_range.end - abs_range.start);
        let flash = AsyncPartitionFlash {
            flash: &mut flash,
            abs_range,
        };
        let config = MapConfig::new(flash_range);
        let mut storage = MapStorage::new(flash, config, NoCache::new());
        let mut data_buffer = [0u8; SHARED_MAP_BUFFER_LEN];
        storage
            .store_item(&mut data_buffer, &key, value)
            .await
            .map_err(|_| ())
    }

    pub async fn remove_key(&self, key: u8) -> Result<(), ()> {
        let mut flash = self.flash.lock().await;
        let abs_range = nvs_tail_window(&mut flash, self.reserved_sectors).ok_or(())?;
        let flash_range = 0..(abs_range.end - abs_range.start);
        let flash = AsyncPartitionFlash {
            flash: &mut flash,
            abs_range,
        };
        let config = MapConfig::new(flash_range);
        let mut storage = MapStorage::new(flash, config, NoCache::new());
        let mut data_buffer = [0u8; SHARED_MAP_BUFFER_LEN];
        storage
            .remove_item(&mut data_buffer, &key)
            .await
            .map_err(|_| ())
    }
}
