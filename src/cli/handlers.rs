extern crate alloc;

use alloc::boxed::Box;
use async_trait::async_trait;
use core::fmt::Write as FmtWrite;
use embedded_io_async::Write as AsyncWrite;

use crate::cli::CommandHandler;
use crate::motor::{
    MOTOR_CMD_CHANNEL, MOTOR_STATUS, MotorCommand, encoder_a_high, encoder_b_high,
    encoder_invalid_transitions, encoder_transitions, encoder_valid_neg_steps,
    encoder_valid_pos_steps, raw_encoder_count, reset_encoder_transitions,
    set_raw_encoder_count,
};

pub struct EchoCommand;

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for EchoCommand
where
    IO: AsyncWrite + FmtWrite,
{
    async fn execute(&self, args: &[&str], io: &mut IO) {
        if args.len() < 2 {
            writeln!(io, "Usage: echo <message>").ok();
            return;
        }

        let mut message = heapless::String::<128>::new();
        for (index, word) in args[1..].iter().enumerate() {
            if index > 0 && message.push(' ').is_err() {
                writeln!(io, "Message too long").ok();
                return;
            }

            if message.push_str(word).is_err() {
                writeln!(io, "Message too long").ok();
                return;
            }
        }

        writeln!(io, "{}", message).ok();
    }
}

pub struct RebootCommand;

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for RebootCommand
where
    IO: AsyncWrite + FmtWrite,
{
    async fn execute(&self, args: &[&str], io: &mut IO) {
        let mode = args.get(1).copied().unwrap_or("normal");

        match mode {
            "normal" => {
                writeln!(io, "Rebooting now...").ok();
                io.flush().await.ok();
                embassy_time::Timer::after_millis(50).await;
                esp_hal::system::software_reset();
            }
            "bootloader" => {
                writeln!(
                    io,
                    "Bootloader mode is not supported via USB Serial/JTAG CLI on ESP32-C3."
                )
                .ok();
                writeln!(io, "Use `cargo espflash` for flashing workflows.").ok();
            }
            _ => {
                writeln!(io, "Usage: reboot [normal|bootloader]").ok();
            }
        }
    }
}

pub struct MotorCommandHandler;

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for MotorCommandHandler
where
    IO: AsyncWrite + FmtWrite,
{
    async fn execute(&self, args: &[&str], io: &mut IO) {
        if args.len() < 2 {
            writeln!(io, "Usage: motor <home|goto|vel|dir|raw|enc|stop|status>").ok();
            return;
        }

        match args[1] {
            "home" => {
                MOTOR_CMD_CHANNEL.send(MotorCommand::Home).await;
                writeln!(io, "motor: homing requested").ok();
            }
            "goto" => {
                let Some(raw) = args.get(2) else {
                    writeln!(io, "Usage: motor goto <position_pct>").ok();
                    return;
                };
                let Ok(target_pct) = raw.parse::<f32>() else {
                    writeln!(io, "Invalid position: {}", raw).ok();
                    return;
                };

                MOTOR_CMD_CHANNEL
                    .send(MotorCommand::SetPosition { target_pct })
                    .await;
                writeln!(io, "motor: target position set to {:.2}%", target_pct).ok();
            }
            "vel" => {
                let Some(raw) = args.get(2) else {
                    writeln!(io, "Usage: motor vel <pct_per_sec>").ok();
                    return;
                };
                let Ok(velocity_pct_per_sec) = raw.parse::<f32>() else {
                    writeln!(io, "Invalid velocity: {}", raw).ok();
                    return;
                };

                MOTOR_CMD_CHANNEL
                    .send(MotorCommand::SetVelocity {
                        velocity_pct_per_sec,
                    })
                    .await;
                writeln!(io, "motor: target velocity set to {:.2}%/s", velocity_pct_per_sec).ok();
            }
            "dir" => {
                let Some(raw) = args.get(2).copied() else {
                    writeln!(io, "Usage: motor dir <normal|reversed|status>").ok();
                    return;
                };

                match raw {
                    "normal" => {
                        MOTOR_CMD_CHANNEL
                            .send(MotorCommand::SetDirectionPolarity {
                                high_is_toward_top: true,
                            })
                            .await;
                        writeln!(io, "motor: direction set to normal (PH high => toward top)").ok();
                    }
                    "reversed" => {
                        MOTOR_CMD_CHANNEL
                            .send(MotorCommand::SetDirectionPolarity {
                                high_is_toward_top: false,
                            })
                            .await;
                        writeln!(io, "motor: direction set to reversed (PH low => toward top)").ok();
                    }
                    "status" => {
                        let status = *MOTOR_STATUS.lock().await;
                        writeln!(
                            io,
                            "motor: direction mapping is {}",
                            if status.direction_high_is_toward_top {
                                "normal (PH high => top)"
                            } else {
                                "reversed (PH low => top)"
                            }
                        )
                        .ok();
                    }
                    _ => {
                        writeln!(io, "Usage: motor dir <normal|reversed|status>").ok();
                    }
                }
            }
            "raw" => {
                let Some(arg2) = args.get(2).copied() else {
                    writeln!(io, "Usage: motor raw <ph:0|1> <en:0|1> | off | status").ok();
                    return;
                };

                match arg2 {
                    "status" => {
                        let status = *MOTOR_STATUS.lock().await;
                        writeln!(io, "raw_mode: {}", status.raw_mode_enabled).ok();
                        writeln!(io, "raw_ph_high: {}", status.raw_ph_high).ok();
                        writeln!(io, "raw_en_high: {}", status.raw_en_high).ok();
                    }
                    "off" => {
                        MOTOR_CMD_CHANNEL.send(MotorCommand::Stop).await;
                        writeln!(io, "motor: raw drive disabled").ok();
                    }
                    ph_raw => {
                        let Some(en_raw) = args.get(3).copied() else {
                            writeln!(io, "Usage: motor raw <ph:0|1> <en:0|1> | off | status").ok();
                            return;
                        };

                        let ph_high = match ph_raw {
                            "0" => false,
                            "1" => true,
                            _ => {
                                writeln!(io, "Invalid ph value: {} (use 0 or 1)", ph_raw).ok();
                                return;
                            }
                        };
                        let en_high = match en_raw {
                            "0" => false,
                            "1" => true,
                            _ => {
                                writeln!(io, "Invalid en value: {} (use 0 or 1)", en_raw).ok();
                                return;
                            }
                        };

                        MOTOR_CMD_CHANNEL
                            .send(MotorCommand::DirectDrive { ph_high, en_high })
                            .await;
                        writeln!(io, "motor: raw drive set ph={} en={}", ph_raw, en_raw).ok();
                    }
                }
            }
            "enc" => {
                if matches!(args.get(2).copied(), Some("reset")) {
                    reset_encoder_transitions();
                    writeln!(io, "motor: encoder diagnostics reset").ok();
                    return;
                }

                if matches!(args.get(2).copied(), Some("zero")) {
                    set_raw_encoder_count(0);
                    writeln!(io, "motor: raw encoder count zeroed").ok();
                    return;
                }

                let status = *MOTOR_STATUS.lock().await;
                writeln!(io, "raw_encoder_count: {}", raw_encoder_count()).ok();
                writeln!(io, "logical_encoder_count: {}", status.logical_encoder_count).ok();
                writeln!(io, "position_clamped_to_limit: {}", status.position_clamped_to_limit).ok();
                writeln!(io, "encoder_a_high: {}", encoder_a_high()).ok();
                writeln!(io, "encoder_b_high: {}", encoder_b_high()).ok();
                writeln!(io, "encoder_transitions: {}", encoder_transitions()).ok();
                writeln!(io, "encoder_valid_pos_steps: {}", encoder_valid_pos_steps()).ok();
                writeln!(io, "encoder_valid_neg_steps: {}", encoder_valid_neg_steps()).ok();
                writeln!(io, "encoder_invalid_transitions: {}", encoder_invalid_transitions()).ok();
            }
            "stop" => {
                MOTOR_CMD_CHANNEL.send(MotorCommand::Stop).await;
                writeln!(io, "motor: stop requested").ok();
            }
            "status" => {
                let status = *MOTOR_STATUS.lock().await;
                writeln!(io, "state: {:?}", status.state).ok();
                writeln!(io, "homed: {}", status.is_homed).ok();
                match status.position_pct {
                    Some(pos) => {
                        writeln!(io, "position: {:.2}%", pos).ok();
                    }
                    None => {
                        writeln!(io, "position: unknown").ok();
                    }
                }
                writeln!(io, "target_position: {:?}", status.target_position_pct).ok();
                writeln!(io, "target_velocity: {:.2}%/s", status.target_velocity_pct_per_sec).ok();
                writeln!(io, "velocity: {:.2}%/s", status.velocity_pct_per_sec).ok();
                writeln!(
                    io,
                    "direction_mapping: {}",
                    if status.direction_high_is_toward_top {
                        "normal (PH high => top)"
                    } else {
                        "reversed (PH low => top)"
                    }
                )
                .ok();
                writeln!(io, "logical_encoder_count: {}", status.logical_encoder_count).ok();
                writeln!(io, "raw_encoder_count: {}", status.raw_encoder_count).ok();
                writeln!(io, "position_clamped_to_limit: {}", status.position_clamped_to_limit).ok();
                writeln!(io, "counts_per_stroke: {:?}", status.counts_per_stroke).ok();
                writeln!(io, "top_limit: {}", status.at_top_limit).ok();
                writeln!(io, "bottom_limit: {}", status.at_bottom_limit).ok();
                writeln!(io, "raw_mode: {}", status.raw_mode_enabled).ok();
                writeln!(io, "raw_ph_high: {}", status.raw_ph_high).ok();
                writeln!(io, "raw_en_high: {}", status.raw_en_high).ok();
                writeln!(io, "fault: {:?}", status.fault_code).ok();
            }
            _ => {
                writeln!(io, "Usage: motor <home|goto|vel|dir|raw|enc|stop|status>").ok();
            }
        }
    }
}
