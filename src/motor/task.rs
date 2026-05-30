use defmt::info;
use embassy_time::{Duration, Ticker};
use esp_hal::gpio::{DriveMode, Input, InputConfig, Level, Output, OutputConfig, Pull};

use super::{
    MOTOR_CMD_CHANNEL, MOTOR_STATUS, MOTOR_STATUS_SIGNAL, MotorCommand, MotorConfig, MotorPins,
    MotorState, TuningState, raw_encoder_count, set_raw_encoder_count,
};
use crate::motor::controller::SpeedController;
use crate::motor::pid_controller::PidController;
use crate::motor::pid_storage::PidTuningStorage;
use crate::motor::pid_tuner::PidTuner;

const MIN_VALID_COUNTS_PER_STROKE: i32 = 20;

fn is_limit_active(input: &Input<'_>, active_low: bool) -> bool {
    if active_low {
        input.is_low()
    } else {
        input.is_high()
    }
}

fn apply_drive(
    direction_high_is_toward_top: bool,
    ph_pin: &mut Output<'_>,
    en_pin: &mut Output<'_>,
    duty_cycle: f32,
    min_run_duty_pct: u8,
    max_run_duty_pct: u8,
    pwm_accumulator: &mut f32,
) {
    // duty_cycle ranges from -1.0 (full reverse) to +1.0 (full forward)
    // Software PWM (pulse-density) so PID output magnitude affects motor effort.
    if duty_cycle.abs() < 0.001 {
        en_pin.set_low();
        *pwm_accumulator = 0.0;
        return;
    }

    let toward_top = duty_cycle > 0.0;
    let dir_high = if direction_high_is_toward_top {
        toward_top
    } else {
        !toward_top
    };

    if dir_high {
        ph_pin.set_high();
    } else {
        ph_pin.set_low();
    }

    let requested = duty_cycle.abs().clamp(0.0, 1.0);
    let min_duty = (min_run_duty_pct as f32 / 100.0).clamp(0.0, 1.0);
    let max_duty = (max_run_duty_pct as f32 / 100.0).clamp(min_duty, 1.0);
    let effective_duty = (min_duty + (max_duty - min_duty) * requested).clamp(0.0, 1.0);

    *pwm_accumulator += effective_duty;
    if *pwm_accumulator >= 1.0 {
        en_pin.set_high();
        *pwm_accumulator -= 1.0;
    } else {
        en_pin.set_low();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HomingPhase {
    SeekBottom,
    SeekTop,
    ReturnBottom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AutotunePhase {
    Idle,
    Centering,
    Relay,
}

#[embassy_executor::task]
pub async fn motor_task(config: MotorConfig, pins: MotorPins, pid_storage: PidTuningStorage) {
    let mut ph_pin = Output::new(
        pins.ph_pin,
        Level::Low,
        OutputConfig::default().with_drive_mode(DriveMode::PushPull),
    );
    let mut en_pin = Output::new(
        pins.en_pin,
        Level::Low,
        OutputConfig::default().with_drive_mode(DriveMode::PushPull),
    );

    let limit_config = if config.limit_switch_active_low {
        InputConfig::default().with_pull(Pull::Up)
    } else {
        InputConfig::default().with_pull(Pull::Down)
    };
    let top_limit = Input::new(pins.top_limit_pin, limit_config);
    let bottom_limit = Input::new(pins.bottom_limit_pin, limit_config);

    let mut status = super::MotorStatus::default();
    let mut counts_per_stroke: Option<i32> = None;
    let mut target_velocity_pct_per_sec = 0.0_f32;
    let mut target_position_pct: Option<f32> = None;
    let mut direction_high_is_toward_top = config.direction_high_is_toward_top;
    let mut homing_phase: Option<HomingPhase> = None;
    let mut direct_drive: Option<(bool, bool)> = None;
    let mut ticker = Ticker::every(Duration::from_millis(config.control_period_ms));

    // Initialize PID controller with default gains
    let loaded_gains = pid_storage.load_from_nvs().await;
    let initial_gains = loaded_gains.unwrap_or_default();
    let mut pid_controller = PidController::new(initial_gains);
    let mut pid_tuner = PidTuner::new();
    let mut autotune_phase = AutotunePhase::Idle;
    let mut pwm_accumulator = 0.0_f32;
    let dt_secs = (config.control_period_ms as f32) / 1000.0;

    status.pid_kp = initial_gains.kp;
    status.pid_ki = initial_gains.ki;
    status.pid_kd = initial_gains.kd;
    status.pid_gains_tuned = loaded_gains.is_some();

    loop {
        ticker.next().await;

        let at_top_limit = is_limit_active(&top_limit, config.limit_switch_active_low);
        let at_bottom_limit = is_limit_active(&bottom_limit, config.limit_switch_active_low);
        let current_raw_encoder_count = raw_encoder_count();
        let current_logical_encoder_count = if status.is_homed {
            if at_bottom_limit {
                0
            } else if at_top_limit {
                counts_per_stroke.unwrap_or(current_raw_encoder_count)
            } else {
                current_raw_encoder_count
            }
        } else {
            current_raw_encoder_count
        };
        let position_clamped_to_limit =
            status.is_homed && (at_bottom_limit || (at_top_limit && counts_per_stroke.is_some()));

        status.at_top_limit = at_top_limit;
        status.at_bottom_limit = at_bottom_limit;
        status.direction_high_is_toward_top = direction_high_is_toward_top;
        status.logical_encoder_count = current_logical_encoder_count;
        status.raw_encoder_count = current_raw_encoder_count;
        status.counts_per_stroke = counts_per_stroke;
        status.position_clamped_to_limit = position_clamped_to_limit;
        status.raw_mode_enabled = direct_drive.is_some();
        status.position_pct = if let Some(stroke) = counts_per_stroke {
            if stroke > 0 {
                Some(
                    ((current_logical_encoder_count as f32 / stroke as f32) * 100.0)
                        .clamp(0.0, 100.0),
                )
            } else if at_bottom_limit {
                Some(config.min_position_pct)
            } else if at_top_limit {
                Some(config.max_position_pct)
            } else {
                None
            }
        } else if at_bottom_limit {
            Some(config.min_position_pct)
        } else if at_top_limit {
            Some(config.max_position_pct)
        } else {
            None
        };

        while let Ok(cmd) = MOTOR_CMD_CHANNEL.try_receive() {
            match cmd {
                MotorCommand::Home => {
                    status.state = MotorState::Homing;
                    status.is_homed = false;
                    status.fault_code = None;
                    counts_per_stroke = None;
                    status.counts_per_stroke = None;
                    direct_drive = None;
                    status.raw_mode_enabled = false;
                    target_position_pct = None;
                    if at_bottom_limit {
                        set_raw_encoder_count(0);
                        homing_phase = Some(HomingPhase::SeekTop);
                    } else {
                        homing_phase = Some(HomingPhase::SeekBottom);
                    }
                    target_velocity_pct_per_sec = 0.0;
                }
                MotorCommand::Stop => {
                    status.state = MotorState::Idle;
                    status.fault_code = None;
                    homing_phase = None;
                    target_velocity_pct_per_sec = 0.0;
                    target_position_pct = None;
                    pid_tuner.abort();
                    status.tuning_state = TuningState::Idle;
                    status.tuning_progress_pct = 0.0;
                    direct_drive = None;
                    status.raw_mode_enabled = false;
                    status.raw_ph_high = false;
                    status.raw_en_high = false;
                }
                MotorCommand::SetPosition { target_pct } => {
                    if counts_per_stroke.is_some() {
                        pid_tuner.abort();
                        autotune_phase = AutotunePhase::Idle;
                        pid_controller.reset();
                        status.tuning_state = TuningState::Idle;
                        status.tuning_progress_pct = 0.0;
                        status.state = MotorState::MovingToPosition;
                        status.fault_code = None;
                        direct_drive = None;
                        target_position_pct = Some(target_pct.clamp(0.0, 100.0));
                    } else {
                        status.fault_code = Some("not_homed");
                    }
                }
                MotorCommand::SetVelocity {
                    velocity_pct_per_sec,
                } => {
                    status.state = MotorState::MovingAtVelocity;
                    status.fault_code = None;
                    homing_phase = None;
                    direct_drive = None;
                    status.raw_mode_enabled = false;
                    target_position_pct = None;
                    target_velocity_pct_per_sec = velocity_pct_per_sec.clamp(
                        -config.max_velocity_pct_per_sec,
                        config.max_velocity_pct_per_sec,
                    );
                }
                MotorCommand::SetPidGains { kp, ki, kd } => {
                    let gains = crate::motor::pid_storage::PidGains { kp, ki, kd };
                    pid_controller.set_gains(gains);
                    if pid_storage.save_to_nvs(gains).await.is_err() {
                        status.fault_code = Some("pid_nvs_save_failed");
                    } else {
                        status.fault_code = None;
                        status.pid_gains_tuned = true;
                    }
                }
                MotorCommand::LoadPidGains => {
                    if let Some(gains) = pid_storage.load_from_nvs().await {
                        pid_controller.set_gains(gains);
                        status.fault_code = None;
                        status.pid_gains_tuned = true;
                    } else {
                        status.fault_code = Some("pid_nvs_load_failed");
                    }
                }
                MotorCommand::ResetPidGains => {
                    let gains = crate::motor::pid_storage::PidGains::default();
                    pid_controller.set_gains(gains);
                    if pid_storage.clear_from_nvs().await.is_err() {
                        status.fault_code = Some("pid_nvs_clear_failed");
                    } else {
                        status.fault_code = None;
                        status.pid_gains_tuned = false;
                    }
                }
                MotorCommand::SetDirectionPolarity { high_is_toward_top } => {
                    direction_high_is_toward_top = high_is_toward_top;
                }
                MotorCommand::DirectDrive { ph_high, en_high } => {
                    status.state = MotorState::RawDrive;
                    status.fault_code = None;
                    homing_phase = None;
                    target_velocity_pct_per_sec = 0.0;
                    direct_drive = Some((ph_high, en_high));
                    status.raw_mode_enabled = true;
                    status.raw_ph_high = ph_high;
                    status.raw_en_high = en_high;
                }
                MotorCommand::StartTuning => {
                    if status.is_homed && status.state == MotorState::Idle {
                        info!("Starting PID auto-tuning");
                        pid_controller.reset();
                        pid_tuner.abort();
                        status.state = MotorState::MovingToPosition;
                        status.tuning_state = TuningState::Idle;
                        status.tuning_progress_pct = 0.0;
                        status.pid_gains_tuned = false;
                        status.fault_code = None;
                        // Center first when starting near stroke ends; this makes tuning representative.
                        let pos = status.position_pct.unwrap_or(50.0);
                        let center_pct = 50.0;
                        let near_end = pos <= 20.0 || pos >= 80.0;
                        if near_end {
                            autotune_phase = AutotunePhase::Centering;
                            target_position_pct = Some(center_pct);
                        } else {
                            autotune_phase = AutotunePhase::Relay;
                            target_position_pct = None;
                            status.tuning_state = TuningState::FindingAmplitude;
                            pid_tuner.start(pos);
                        }
                    } else {
                        status.fault_code = Some("cannot_tune_not_homed_or_idle");
                    }
                }
                MotorCommand::AbortTuning => {
                    if status.tuning_state != TuningState::Idle {
                        info!("Aborting PID auto-tuning");
                        pid_tuner.abort();
                        autotune_phase = AutotunePhase::Idle;
                        status.tuning_state = TuningState::Idle;
                        status.state = MotorState::Idle;
                        target_position_pct = None;
                    }
                }
            }
        }

        let mut commanded_velocity = 0.0_f32;

        if let Some((ph_high, en_high)) = direct_drive {
            if ph_high {
                ph_pin.set_high();
            } else {
                ph_pin.set_low();
            }
            if en_high {
                en_pin.set_high();
            } else {
                en_pin.set_low();
            }
            status.velocity_pct_per_sec = 0.0;
            status.raw_mode_enabled = true;
            status.raw_ph_high = ph_high;
            status.raw_en_high = en_high;
        } else {
            match status.state {
                MotorState::Idle => {
                    commanded_velocity = 0.0;
                }
                MotorState::Homing => match homing_phase {
                    Some(HomingPhase::SeekBottom) => {
                        if at_bottom_limit {
                            set_raw_encoder_count(0);
                            status.logical_encoder_count = 0;
                            status.raw_encoder_count = 0;
                            homing_phase = Some(HomingPhase::SeekTop);
                            commanded_velocity = 0.0;
                        } else {
                            commanded_velocity = -config.home_velocity_pct_per_sec;
                        }
                    }
                    Some(HomingPhase::SeekTop) => {
                        if at_top_limit {
                            let stroke = raw_encoder_count().abs();
                            if stroke >= MIN_VALID_COUNTS_PER_STROKE {
                                counts_per_stroke = Some(stroke);
                                status.counts_per_stroke = Some(stroke);
                                homing_phase = Some(HomingPhase::ReturnBottom);
                            } else {
                                status.state = MotorState::Fault;
                                status.fault_code = Some("encoder_not_counting");
                                counts_per_stroke = None;
                                status.counts_per_stroke = None;
                                homing_phase = None;
                            }
                            commanded_velocity = 0.0;
                        } else {
                            commanded_velocity = config.home_velocity_pct_per_sec;
                        }
                    }
                    Some(HomingPhase::ReturnBottom) => {
                        if at_bottom_limit {
                            set_raw_encoder_count(0);
                            status.logical_encoder_count = 0;
                            status.raw_encoder_count = 0;
                            status.is_homed = true;
                            status.state = MotorState::Idle;
                            homing_phase = None;
                            commanded_velocity = 0.0;
                        } else {
                            commanded_velocity = -config.home_velocity_pct_per_sec;
                        }
                    }
                    None => {
                        status.state = MotorState::Idle;
                    }
                },
                MotorState::MovingToPosition => {
                    let current_pct = if let Some(stroke) = counts_per_stroke {
                        if stroke > 0 {
                            ((current_logical_encoder_count as f32 / stroke as f32) * 100.0)
                                .clamp(0.0, 100.0)
                        } else {
                            0.0
                        }
                    } else {
                        0.0
                    };

                    // If tuning, execute relay feedback (regardless of target_position_pct)
                    if autotune_phase == AutotunePhase::Centering {
                        let center_error = 50.0 - current_pct;
                        let center_band = 2.0;
                        if center_error.abs() <= center_band {
                            autotune_phase = AutotunePhase::Relay;
                            target_position_pct = None;
                            status.tuning_state = TuningState::FindingAmplitude;
                            status.tuning_progress_pct = 0.0;
                            pid_controller.reset();
                            pid_tuner.start(current_pct);
                            status.velocity_pct_per_sec = 0.0;
                        }
                    }

                    if autotune_phase == AutotunePhase::Relay
                        && matches!(
                            status.tuning_state,
                            TuningState::FindingAmplitude | TuningState::MeasuringPeriod
                        )
                    {
                        status.tuning_progress_pct = pid_tuner.progress_percent();
                        let (relay_duty, tuning_complete) = pid_tuner.step(current_pct);
                        commanded_velocity = relay_duty * config.max_velocity_pct_per_sec;

                        if tuning_complete {
                            if let Some(gains) = pid_tuner.calculate_gains() {
                                info!(
                                    "Tuning complete: Kp={}, Ki={}, Kd={}",
                                    gains.kp, gains.ki, gains.kd
                                );
                                pid_controller.set_gains(gains);
                                if pid_storage.save_to_nvs(gains).await.is_err() {
                                    status.fault_code = Some("pid_nvs_save_failed");
                                }
                                status.tuning_state = TuningState::Complete;
                                status.tuning_progress_pct = 100.0;
                                status.pid_gains_tuned = true;
                                pid_tuner.abort();
                                autotune_phase = AutotunePhase::Idle;
                            } else {
                                status.tuning_state = TuningState::Failed;
                                status.tuning_progress_pct = 100.0;
                                pid_tuner.abort();
                                autotune_phase = AutotunePhase::Idle;
                            }
                            status.state = MotorState::Idle;
                            target_position_pct = None;
                            commanded_velocity = 0.0;
                        }
                        status.velocity_pct_per_sec = commanded_velocity;
                    } else if let (Some(target_pct), Some(stroke)) =
                        (target_position_pct, counts_per_stroke)
                    {
                        // Normal position control with PID
                        if stroke < MIN_VALID_COUNTS_PER_STROKE {
                            status.state = MotorState::Fault;
                            status.fault_code = Some("encoder_not_counting");
                            counts_per_stroke = None;
                            status.counts_per_stroke = None;
                            target_position_pct = None;
                            status.velocity_pct_per_sec = 0.0;
                            commanded_velocity = 0.0;
                        } else {
                            let error_pct = target_pct - current_pct;

                            // Use PID controller to compute duty cycle
                            let mut duty_cycle = pid_controller.compute(error_pct, 0.0, dt_secs);

                            // Distance-based cruise floor for better full-stroke move time.
                            let abs_error = error_pct.abs();
                            let min_cruise = if abs_error > 30.0 {
                                0.80
                            } else if abs_error > 15.0 {
                                0.60
                            } else if abs_error > 6.0 {
                                0.35
                            } else {
                                0.0
                            };
                            if min_cruise > 0.0 {
                                let sign = if error_pct >= 0.0 { 1.0 } else { -1.0 };
                                duty_cycle = sign * duty_cycle.abs().max(min_cruise);
                            }

                            // Decel envelope near target to avoid overshoot.
                            let speed_scale = (abs_error / 12.0).clamp(0.25, 1.0);
                            commanded_velocity = duty_cycle * config.max_velocity_pct_per_sec;
                            commanded_velocity *= speed_scale;

                            // Check if settled (use a tighter band for PID)
                            let settle_band = 0.2; // Tighter than position_deadband_pct
                            if error_pct.abs() <= settle_band && commanded_velocity.abs() < 1.0 {
                                status.state = MotorState::Idle;
                                status.velocity_pct_per_sec = 0.0;
                                target_position_pct = None;
                                commanded_velocity = 0.0;
                                pid_controller.reset();
                            } else {
                                status.velocity_pct_per_sec = commanded_velocity;
                            }
                        }
                    } else {
                        status.state = MotorState::Idle;
                        status.velocity_pct_per_sec = 0.0;
                        target_position_pct = None;
                        commanded_velocity = 0.0;
                    }
                }
                MotorState::MovingAtVelocity => {
                    commanded_velocity = target_velocity_pct_per_sec;
                    status.velocity_pct_per_sec = target_velocity_pct_per_sec;
                }
                MotorState::Fault => {
                    target_velocity_pct_per_sec = 0.0;
                    status.velocity_pct_per_sec = 0.0;
                    commanded_velocity = 0.0;
                }
                MotorState::RawDrive => {
                    // Raw mode is always handled by direct_drive branch above.
                    status.state = MotorState::Idle;
                    commanded_velocity = 0.0;
                }
            }

            if at_bottom_limit {
                commanded_velocity = commanded_velocity.max(0.0);
                if status.state == MotorState::MovingAtVelocity && commanded_velocity == 0.0 {
                    status.state = MotorState::Idle;
                    target_velocity_pct_per_sec = 0.0;
                    status.velocity_pct_per_sec = 0.0;
                }
                if status.state == MotorState::MovingToPosition && commanded_velocity == 0.0 {
                    target_position_pct = None;
                    status.state = MotorState::Idle;
                    status.velocity_pct_per_sec = 0.0;
                }
            }

            if at_top_limit {
                commanded_velocity = commanded_velocity.min(0.0);
                if status.state == MotorState::MovingAtVelocity && commanded_velocity == 0.0 {
                    status.state = MotorState::Idle;
                    target_velocity_pct_per_sec = 0.0;
                    status.velocity_pct_per_sec = 0.0;
                }
                if status.state == MotorState::MovingToPosition && commanded_velocity == 0.0 {
                    target_position_pct = None;
                    status.state = MotorState::Idle;
                    status.velocity_pct_per_sec = 0.0;
                }
            }

            apply_drive(
                direction_high_is_toward_top,
                &mut ph_pin,
                &mut en_pin,
                (commanded_velocity / config.max_velocity_pct_per_sec).clamp(-1.0, 1.0),
                config.min_run_duty_pct,
                config.max_run_duty_pct,
                &mut pwm_accumulator,
            );

            status.raw_mode_enabled = false;
            status.raw_ph_high = false;
            status.raw_en_high = false;
        }

        status.target_position_pct = target_position_pct;
        status.target_velocity_pct_per_sec = target_velocity_pct_per_sec;
        let gains = pid_controller.gains();
        status.pid_kp = gains.kp;
        status.pid_ki = gains.ki;
        status.pid_kd = gains.kd;

        {
            let mut guard = MOTOR_STATUS.lock().await;
            *guard = status;
        }
        MOTOR_STATUS_SIGNAL.signal(status);
    }
}
