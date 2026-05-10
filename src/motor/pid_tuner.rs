use core::f32;

use crate::motor::pid_storage::PidGains;
use defmt::{info, warn};

/// State of the PID tuning process
#[derive(Clone, Copy, Debug, PartialEq, Eq, defmt::Format)]
pub enum TuningState {
    Idle,
    FindingAmplitude,
    MeasuringPeriod,
    Complete,
    Failed,
}

/// Relay auto-tuning for PID controller
/// Uses Åström-Hägglund relay feedback method to estimate critical gain and period
pub struct PidTuner {
    state: TuningState,
    relay_amplitude: f32, // Duty cycle amplitude for relay (e.g., 0.5 = 50%)
    position_history: heapless::Vec<f32, 256>, // Store position samples
    tune_center_position: f32,
    hysteresis_pct: f32,
    relay_output: f32,
    zero_crossings: i32, // Count zero crossings to detect period
    cycle_count: i32,    // Track number of complete oscillations
    max_position: f32,
    min_position: f32,
    last_deviation: f32,
    last_crossing_step: i32,
    half_period_steps_accum: i32,
    half_period_samples: i32,
    control_cycles_elapsed: i32,
    max_control_cycles: i32, // Safety limit (~10 seconds at 10ms cycles)
}

impl Default for PidTuner {
    fn default() -> Self {
        Self::new()
    }
}

impl PidTuner {
    pub fn new() -> Self {
        Self {
            state: TuningState::Idle,
            relay_amplitude: 0.5, // 50% duty
            position_history: heapless::Vec::new(),
            tune_center_position: 0.0,
            hysteresis_pct: 0.2,
            relay_output: 0.5,
            zero_crossings: 0,
            cycle_count: 0,
            max_position: f32::NEG_INFINITY,
            min_position: f32::INFINITY,
            last_deviation: 0.0,
            last_crossing_step: 0,
            half_period_steps_accum: 0,
            half_period_samples: 0,
            control_cycles_elapsed: 0,
            max_control_cycles: 1000, // ~10 seconds at 10ms period
        }
    }

    pub fn state(&self) -> TuningState {
        self.state
    }

    pub fn progress_percent(&self) -> f32 {
        let progress =
            (self.control_cycles_elapsed as f32 / self.max_control_cycles as f32) * 100.0;
        progress.min(99.0) // Cap at 99% until complete
    }

    pub fn start(&mut self, current_position: f32) {
        self.state = TuningState::FindingAmplitude;
        self.position_history.clear();
        let _ = self.position_history.push(current_position);
        self.tune_center_position = current_position;
        self.last_deviation = 0.0;
        self.relay_output = self.relay_amplitude;
        self.max_position = current_position;
        self.min_position = current_position;
        self.zero_crossings = 0;
        self.cycle_count = 0;
        self.last_crossing_step = 0;
        self.half_period_steps_accum = 0;
        self.half_period_samples = 0;
        self.control_cycles_elapsed = 0;
        info!("PID tuning started");
    }

    /// Step the tuning process and return the relay duty cycle output
    /// position: current motor position in %
    /// Returns: (duty_cycle_output, tuning_complete)
    pub fn step(&mut self, position: f32) -> (f32, bool) {
        self.control_cycles_elapsed += 1;

        // Store position history
        let _ = self.position_history.push(position);

        // Track amplitude bounds
        self.max_position = self.max_position.max(position);
        self.min_position = self.min_position.min(position);

        // Detect crossings around tune center with hysteresis
        let deviation = position - self.tune_center_position;
        let crossed_up =
            self.last_deviation <= -self.hysteresis_pct && deviation >= self.hysteresis_pct;
        let crossed_down =
            self.last_deviation >= self.hysteresis_pct && deviation <= -self.hysteresis_pct;
        if crossed_up || crossed_down {
            self.zero_crossings += 1;
            if self.last_crossing_step > 0 {
                let half_period_steps = self.control_cycles_elapsed - self.last_crossing_step;
                if half_period_steps > 0 {
                    self.half_period_steps_accum += half_period_steps;
                    self.half_period_samples += 1;
                }
            }
            self.last_crossing_step = self.control_cycles_elapsed;
            self.cycle_count = self.zero_crossings / 2;
            if self.zero_crossings >= 2 {
                self.state = TuningState::MeasuringPeriod;
            }
        }
        self.last_deviation = deviation;

        // Relay output with hysteresis to avoid chattering near center
        if deviation >= self.hysteresis_pct {
            self.relay_output = -self.relay_amplitude;
        } else if deviation <= -self.hysteresis_pct {
            self.relay_output = self.relay_amplitude;
        }

        // Finish successfully once enough oscillation data is collected
        let peak_to_peak = self.max_position - self.min_position;
        if self.cycle_count >= 4 && self.half_period_samples >= 6 && peak_to_peak >= 0.5 {
            self.state = TuningState::Complete;
        }

        // Tuning timeout or complete
        if self.control_cycles_elapsed >= self.max_control_cycles {
            if self.cycle_count >= 4 {
                // Collected enough data
                self.state = TuningState::Complete;
            } else {
                warn!("Tuning timeout: insufficient oscillations detected");
                self.state = TuningState::Failed;
            }
        }

        let complete = self.state == TuningState::Complete || self.state == TuningState::Failed;
        (self.relay_output, complete)
    }

    /// Calculate PID gains from measured data
    /// Returns tuned gains if successful
    pub fn calculate_gains(&self) -> Option<PidGains> {
        if self.state != TuningState::Complete {
            return None;
        }

        let amplitude = (self.max_position - self.min_position) / 2.0;
        if amplitude < 0.1 {
            warn!("Tuning amplitude too small: {}", amplitude);
            return None;
        }

        // Calculate critical gain from relay amplitude d and oscillation amplitude a.
        // K_u = (4 * d) / (π * a)
        // where a is half of peak-to-peak amplitude.
        let k_u = (4.0 * self.relay_amplitude) / (f32::consts::PI * amplitude);

        if self.half_period_samples <= 0 {
            warn!("Invalid tuning period samples");
            return None;
        }

        // Average half-period converted to full period in seconds.
        let avg_half_period_steps =
            self.half_period_steps_accum as f32 / self.half_period_samples as f32;
        let t_u = 2.0 * avg_half_period_steps * 0.01; // 10ms control period

        if t_u <= 0.0 || k_u <= 0.0 {
            warn!("Invalid tuning parameters: K_u={}, T_u={}", k_u, t_u);
            return None;
        }

        // Conservative no-overshoot gains (more damped than classic Z-N PID).
        let kp = 0.15 * k_u;
        let ki = 0.30 * k_u / t_u;
        let kd = 0.10 * k_u * t_u;

        info!(
            "Tuning complete: K_u={}, T_u={}, Kp={}, Ki={}, Kd={}",
            k_u, t_u, kp, ki, kd
        );

        Some(PidGains { kp, ki, kd })
    }

    pub fn abort(&mut self) {
        self.state = TuningState::Idle;
        self.position_history.clear();
    }
}
