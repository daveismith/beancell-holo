use super::controller::SpeedController;
use crate::motor::pid_storage::PidGains;
use defmt::info;

/// PID controller for smooth motor position control
#[derive(Clone, Copy, Debug)]
pub struct PidController {
    gains: PidGains,
    integral_error: f32,
    last_error: f32,
    integral_clamp: f32,
}

impl PidController {
    pub fn new(gains: PidGains) -> Self {
        Self {
            gains,
            integral_error: 0.0,
            last_error: 0.0,
            integral_clamp: 100.0, // Clamp integral term to ±100% duty
        }
    }

    pub fn set_gains(&mut self, gains: PidGains) {
        self.gains = gains;
        info!("PID gains updated: Kp={}, Ki={}, Kd={}", gains.kp, gains.ki, gains.kd);
    }

    pub fn reset(&mut self) {
        self.integral_error = 0.0;
        self.last_error = 0.0;
    }

    pub fn gains(&self) -> PidGains {
        self.gains
    }
}

impl SpeedController for PidController {
    fn compute(&mut self, position_error_pct: f32, _current_velocity_pct_per_sec: f32, dt_secs: f32) -> f32 {
        // Proportional term
        let p_term = self.gains.kp * position_error_pct;

        // Integral term with anti-windup
        // Only integrate if P term is not saturated at ±100% duty
        if p_term.abs() < 100.0 {
            self.integral_error += position_error_pct * dt_secs;
            // Clamp integral to prevent windup
            self.integral_error = self
                .integral_error
                .clamp(-self.integral_clamp, self.integral_clamp);
        } else {
            // If saturated, slowly decay the integral to avoid windup
            self.integral_error *= 0.95;
        }
        let i_term = self.gains.ki * self.integral_error;

        // Derivative term (derivative of error)
        let error_rate = (position_error_pct - self.last_error) / dt_secs;
        let d_term = self.gains.kd * error_rate;

        self.last_error = position_error_pct;

        // Combine all terms and clamp to duty cycle range [-1.0, 1.0]
        let output = (p_term + i_term + d_term) / 100.0;
        output.clamp(-1.0, 1.0)
    }

    fn reset(&mut self) {
        self.reset();
    }
}
