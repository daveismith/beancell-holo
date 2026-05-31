use defmt::info;

use crate::shared_flash::{SharedFlash, SharedMapStorage};

const PID_KEY: u8 = 1;
const PID_RESERVED_SECTORS: u32 = 2;

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

pub struct PidTuningStorage {
    map: SharedMapStorage,
}

impl PidTuningStorage {
    pub const fn new(flash: &'static SharedFlash) -> Self {
        Self {
            map: SharedMapStorage::new(flash, PID_RESERVED_SECTORS),
        }
    }

    /// Load PID gains from non-volatile flash.
    pub async fn load_from_nvs(&self) -> Option<PidGains> {
        let stored = self.map.fetch_value::<[f32; 3]>(PID_KEY).await.ok()??;

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
        self.map
            .store_value(PID_KEY, &[gains.kp, gains.ki, gains.kd])
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
        self.map.remove_key(PID_KEY).await.map_err(|_| ())?;
        info!("Cleared PID gains from flash");
        Ok(())
    }
}
