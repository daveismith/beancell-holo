use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use esp_hal::gpio::AnyPin;
use portable_atomic::{AtomicI32, AtomicU8, Ordering};

pub mod controller;
pub mod driver;
pub mod encoder;
pub mod task;
pub mod pid_controller;
pub mod pid_storage;
pub mod pid_tuner;

pub const MOTOR_CMD_QUEUE_DEPTH: usize = 8;

#[derive(Clone, Copy, Debug)]
pub struct MotorConfig {
    pub min_position_pct: f32,
    pub max_position_pct: f32,
    pub max_velocity_pct_per_sec: f32,
    pub position_deadband_pct: f32,
    pub home_velocity_pct_per_sec: f32,
    pub control_period_ms: u64,
    pub pwm_frequency_hz: u32,
    pub min_run_duty_pct: u8,
    pub max_run_duty_pct: u8,
    pub limit_switch_active_low: bool,
    pub direction_high_is_toward_top: bool,
}

impl Default for MotorConfig {
    fn default() -> Self {
        Self {
            min_position_pct: 0.0,
            max_position_pct: 100.0,
            max_velocity_pct_per_sec: 100.0,
            position_deadband_pct: 0.5,
            home_velocity_pct_per_sec: 15.0,
            control_period_ms: 10,
            pwm_frequency_hz: 20_000,
            min_run_duty_pct: 15,
            max_run_duty_pct: 95,
            limit_switch_active_low: true,
            direction_high_is_toward_top: false,
        }
    }
}

#[derive(Debug)]
pub struct MotorPins {
    pub en_pin: AnyPin<'static>,
    pub ph_pin: AnyPin<'static>,
    pub top_limit_pin: AnyPin<'static>,
    pub bottom_limit_pin: AnyPin<'static>,
}

#[derive(Debug)]
pub struct EncoderPins {
    pub channel_a_pin: AnyPin<'static>,
    pub channel_b_pin: AnyPin<'static>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncoderInputPull {
    None,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug)]
pub struct EncoderConfig {
    pub input_pull: EncoderInputPull,
    pub swap_channels: bool,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            input_pull: EncoderInputPull::None,
            swap_channels: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionDirection {
    TowardBottom,
    TowardTop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, defmt::Format)]
pub enum TuningState {
    Idle,
    FindingAmplitude,
    MeasuringPeriod,
    Complete,
    Failed,
}

#[derive(Clone, Copy, Debug)]
pub enum MotorCommand {
    Home,
    Stop,
    SetPosition { target_pct: f32 },
    SetVelocity { velocity_pct_per_sec: f32 },
    SetPidGains { kp: f32, ki: f32, kd: f32 },
    LoadPidGains,
    ResetPidGains,
    SetDirectionPolarity { high_is_toward_top: bool },
    DirectDrive { ph_high: bool, en_high: bool },
    StartTuning,
    AbortTuning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotorState {
    Idle,
    Homing,
    MovingToPosition,
    MovingAtVelocity,
    RawDrive,
    Fault,
}

#[derive(Clone, Copy, Debug)]
pub struct MotorStatus {
    pub state: MotorState,
    pub is_homed: bool,
    pub position_pct: Option<f32>,
    pub target_position_pct: Option<f32>,
    pub target_velocity_pct_per_sec: f32,
    pub velocity_pct_per_sec: f32,
    pub direction_high_is_toward_top: bool,
    pub logical_encoder_count: i32,
    pub raw_encoder_count: i32,
    pub counts_per_stroke: Option<i32>,
    pub at_top_limit: bool,
    pub at_bottom_limit: bool,
    pub position_clamped_to_limit: bool,
    pub raw_mode_enabled: bool,
    pub raw_ph_high: bool,
    pub raw_en_high: bool,
    pub fault_code: Option<&'static str>,
    pub tuning_state: TuningState,
    pub tuning_progress_pct: f32,
    pub pid_kp: f32,
    pub pid_ki: f32,
    pub pid_kd: f32,
    pub pid_gains_tuned: bool,
}

impl Default for MotorStatus {
    fn default() -> Self {
        Self {
            state: MotorState::Idle,
            is_homed: false,
            position_pct: None,
            target_position_pct: None,
            target_velocity_pct_per_sec: 0.0,
            velocity_pct_per_sec: 0.0,
            direction_high_is_toward_top: false,
            logical_encoder_count: 0,
            raw_encoder_count: 0,
            counts_per_stroke: None,
            at_top_limit: false,
            at_bottom_limit: false,
            position_clamped_to_limit: false,
            raw_mode_enabled: false,
            raw_ph_high: false,
            raw_en_high: false,
            fault_code: None,
            tuning_state: TuningState::Idle,
            tuning_progress_pct: 0.0,
            pid_kp: 0.5,
            pid_ki: 0.01,
            pid_kd: 0.1,
            pid_gains_tuned: false,
        }
    }
}

pub static MOTOR_CMD_CHANNEL: Channel<CriticalSectionRawMutex, MotorCommand, MOTOR_CMD_QUEUE_DEPTH> =
    Channel::new();
pub static MOTOR_STATUS_SIGNAL: Signal<CriticalSectionRawMutex, MotorStatus> = Signal::new();
pub static MOTOR_STATUS: Mutex<CriticalSectionRawMutex, MotorStatus> = Mutex::new(MotorStatus {
    state: MotorState::Idle,
    is_homed: false,
    position_pct: None,
    target_position_pct: None,
    target_velocity_pct_per_sec: 0.0,
    velocity_pct_per_sec: 0.0,
    direction_high_is_toward_top: false,
    logical_encoder_count: 0,
    raw_encoder_count: 0,
    counts_per_stroke: None,
    at_top_limit: false,
    at_bottom_limit: false,
    position_clamped_to_limit: false,
    raw_mode_enabled: false,
    raw_ph_high: false,
    raw_en_high: false,
    fault_code: None,
    tuning_state: TuningState::Idle,
    tuning_progress_pct: 0.0,
    pid_kp: 0.5,
    pid_ki: 0.01,
    pid_kd: 0.1,
    pid_gains_tuned: false,
});
pub static MOTOR_RAW_ENCODER_COUNT: AtomicI32 = AtomicI32::new(0);
pub static MOTOR_ENCODER_A_LEVEL: AtomicU8 = AtomicU8::new(0);
pub static MOTOR_ENCODER_B_LEVEL: AtomicU8 = AtomicU8::new(0);
pub static MOTOR_ENCODER_TRANSITIONS: AtomicI32 = AtomicI32::new(0);
pub static MOTOR_ENCODER_VALID_POS_STEPS: AtomicI32 = AtomicI32::new(0);
pub static MOTOR_ENCODER_VALID_NEG_STEPS: AtomicI32 = AtomicI32::new(0);
pub static MOTOR_ENCODER_INVALID_TRANSITIONS: AtomicI32 = AtomicI32::new(0);

pub fn raw_encoder_count() -> i32 {
    MOTOR_RAW_ENCODER_COUNT.load(Ordering::Relaxed)
}

pub fn set_raw_encoder_count(count: i32) {
    MOTOR_RAW_ENCODER_COUNT.store(count, Ordering::Relaxed);
}

pub fn add_raw_encoder_count(delta: i32) {
    MOTOR_RAW_ENCODER_COUNT.fetch_add(delta, Ordering::Relaxed);
}

pub fn set_encoder_levels(a_high: bool, b_high: bool) {
    MOTOR_ENCODER_A_LEVEL.store(if a_high { 1 } else { 0 }, Ordering::Relaxed);
    MOTOR_ENCODER_B_LEVEL.store(if b_high { 1 } else { 0 }, Ordering::Relaxed);
}

pub fn encoder_a_high() -> bool {
    MOTOR_ENCODER_A_LEVEL.load(Ordering::Relaxed) != 0
}

pub fn encoder_b_high() -> bool {
    MOTOR_ENCODER_B_LEVEL.load(Ordering::Relaxed) != 0
}

pub fn reset_encoder_transitions() {
    MOTOR_ENCODER_TRANSITIONS.store(0, Ordering::Relaxed);
    MOTOR_ENCODER_VALID_POS_STEPS.store(0, Ordering::Relaxed);
    MOTOR_ENCODER_VALID_NEG_STEPS.store(0, Ordering::Relaxed);
    MOTOR_ENCODER_INVALID_TRANSITIONS.store(0, Ordering::Relaxed);
}

pub fn add_encoder_transition() {
    MOTOR_ENCODER_TRANSITIONS.fetch_add(1, Ordering::Relaxed);
}

pub fn encoder_transitions() -> i32 {
    MOTOR_ENCODER_TRANSITIONS.load(Ordering::Relaxed)
}

pub fn encoder_valid_pos_steps() -> i32 {
    MOTOR_ENCODER_VALID_POS_STEPS.load(Ordering::Relaxed)
}

pub fn encoder_valid_neg_steps() -> i32 {
    MOTOR_ENCODER_VALID_NEG_STEPS.load(Ordering::Relaxed)
}

pub fn encoder_invalid_transitions() -> i32 {
    MOTOR_ENCODER_INVALID_TRANSITIONS.load(Ordering::Relaxed)
}
