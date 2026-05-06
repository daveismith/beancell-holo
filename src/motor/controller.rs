pub trait SpeedController {
    fn compute(&mut self, position_error_pct: f32, current_velocity_pct_per_sec: f32, dt_secs: f32) -> f32;
    fn reset(&mut self);
}

#[derive(Clone, Copy, Debug)]
pub struct BangBangController {
    pub low_duty: f32,
    pub high_duty: f32,
    pub settle_band_pct: f32,
    pub slow_band_pct: f32,
}

impl Default for BangBangController {
    fn default() -> Self {
        Self {
            low_duty: 0.25,
            high_duty: 0.8,
            settle_band_pct: 0.5,
            slow_band_pct: 5.0,
        }
    }
}

impl SpeedController for BangBangController {
    fn compute(&mut self, position_error_pct: f32, _current_velocity_pct_per_sec: f32, _dt_secs: f32) -> f32 {
        let abs_error = position_error_pct.abs();
        if abs_error <= self.settle_band_pct {
            return 0.0;
        }

        let mag = if abs_error <= self.slow_band_pct {
            self.low_duty
        } else {
            self.high_duty
        };

        if position_error_pct >= 0.0 {
            mag
        } else {
            -mag
        }
    }

    fn reset(&mut self) {}
}
