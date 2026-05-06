use embassy_time::{Duration, Ticker};
use esp_hal::gpio::{DriveMode, Input, InputConfig, Level, Output, OutputConfig, Pull};

use super::{
    MOTOR_CMD_CHANNEL, MOTOR_STATUS, MOTOR_STATUS_SIGNAL, MotorCommand, MotorConfig, MotorPins,
    MotorState, encoder_count, set_encoder_count,
};

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
    velocity_pct_per_sec: f32,
) {
    if velocity_pct_per_sec.abs() < 0.001 {
        en_pin.set_low();
        return;
    }

    let toward_top = velocity_pct_per_sec > 0.0;
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

    // Direct GPIO drive for EN pin: bring-up mode (on/off, no PWM).
    en_pin.set_high();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HomingPhase {
    SeekBottom,
    SeekTop,
    ReturnBottom,
}

#[embassy_executor::task]
pub async fn motor_task(config: MotorConfig, pins: MotorPins) {
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

    loop {
        ticker.next().await;

        let at_top_limit = is_limit_active(&top_limit, config.limit_switch_active_low);
        let at_bottom_limit = is_limit_active(&bottom_limit, config.limit_switch_active_low);
        let mut current_encoder_count = encoder_count();

        status.at_top_limit = at_top_limit;
        status.at_bottom_limit = at_bottom_limit;
        status.direction_high_is_toward_top = direction_high_is_toward_top;
        status.encoder_count = current_encoder_count;
        status.counts_per_stroke = counts_per_stroke;
        status.raw_mode_enabled = direct_drive.is_some();
        status.position_pct = if let Some(stroke) = counts_per_stroke {
            if stroke > 0 {
                Some(((current_encoder_count as f32 / stroke as f32) * 100.0).clamp(0.0, 100.0))
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
                    direct_drive = None;
                    status.raw_mode_enabled = false;
                    target_position_pct = None;
                    if at_bottom_limit {
                        set_encoder_count(0);
                        current_encoder_count = 0;
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
                    direct_drive = None;
                    status.raw_mode_enabled = false;
                    status.raw_ph_high = false;
                    status.raw_en_high = false;
                }
                MotorCommand::SetPosition { target_pct } => {
                    if counts_per_stroke.is_some() {
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
                    target_velocity_pct_per_sec = velocity_pct_per_sec
                        .clamp(-config.max_velocity_pct_per_sec, config.max_velocity_pct_per_sec);
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
                        set_encoder_count(0);
                        current_encoder_count = 0;
                        status.encoder_count = 0;
                        homing_phase = Some(HomingPhase::SeekTop);
                        commanded_velocity = 0.0;
                    } else {
                        commanded_velocity = -config.home_velocity_pct_per_sec;
                    }
                }
                Some(HomingPhase::SeekTop) => {
                    if at_top_limit {
                        let stroke = current_encoder_count.abs();
                        if stroke > 0 {
                            counts_per_stroke = Some(stroke);
                            status.counts_per_stroke = Some(stroke);
                            homing_phase = Some(HomingPhase::ReturnBottom);
                        } else {
                            status.state = MotorState::Fault;
                            status.fault_code = Some("invalid_homing_stroke");
                            homing_phase = None;
                        }
                        commanded_velocity = 0.0;
                    } else {
                        commanded_velocity = config.home_velocity_pct_per_sec;
                    }
                }
                Some(HomingPhase::ReturnBottom) => {
                    if at_bottom_limit {
                        set_encoder_count(0);
                        status.encoder_count = 0;
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
                if let (Some(target_pct), Some(stroke)) = (target_position_pct, counts_per_stroke) {
                    let current_pct = ((current_encoder_count as f32 / stroke as f32) * 100.0)
                        .clamp(0.0, 100.0);
                    let error_pct = target_pct - current_pct;
                    if error_pct.abs() <= config.position_deadband_pct {
                        status.state = MotorState::Idle;
                        status.velocity_pct_per_sec = 0.0;
                        target_position_pct = None;
                        commanded_velocity = 0.0;
                    } else if error_pct > 0.0 {
                        commanded_velocity = config.max_velocity_pct_per_sec;
                        status.velocity_pct_per_sec = commanded_velocity;
                    } else {
                        commanded_velocity = -config.max_velocity_pct_per_sec;
                        status.velocity_pct_per_sec = commanded_velocity;
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
                commanded_velocity,
            );

            status.raw_mode_enabled = false;
            status.raw_ph_high = false;
            status.raw_en_high = false;
        }

        status.target_position_pct = target_position_pct;
        status.target_velocity_pct_per_sec = target_velocity_pct_per_sec;

        {
            let mut guard = MOTOR_STATUS.lock().await;
            *guard = status;
        }
        MOTOR_STATUS_SIGNAL.signal(status);
    }
}
