use defmt::{info, warn};

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

/// NVS storage wrapper for PID gains
/// Note: NVS initialization requires esp-idf-svc which is conditionally compiled
pub struct PidTuningStorage {
    _namespace: &'static str,
    _key: &'static str,
}

impl PidTuningStorage {
    pub const fn new() -> Self {
        Self {
            _namespace: "pid",
            _key: "gains",
        }
    }

    /// Load PID gains from NVS (when esp-idf-svc is available)
    /// For now, always returns None - storage is deferred
    pub fn load_from_nvs(&self) -> Option<PidGains> {
        warn!("NVS storage not yet configured; using default PID gains");
        None
    }

    /// Save PID gains to NVS (when esp-idf-svc is available)
    /// For now, this is a no-op
    pub fn save_to_nvs(&self, gains: PidGains) -> Result<(), ()> {
        info!("PID gains would be saved to NVS: Kp={}, Ki={}, Kd={}", gains.kp, gains.ki, gains.kd);
        // TODO: Implement when NVS is available
        Ok(())
    }

    /// Clear PID gains from NVS (when esp-idf-svc is available)
    pub fn clear_from_nvs(&self) -> Result<(), ()> {
        info!("Cleared PID gains from NVS");
        // TODO: Implement when NVS is available
        Ok(())
    }
}
