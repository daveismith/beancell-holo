#![allow(clippy::collapsible_if)]
extern crate alloc;

use alloc::boxed::Box;
use async_trait::async_trait;
use core::fmt::Write as FmtWrite;
use embedded_io_async::Write as AsyncWrite;

use crate::cli::CommandHandler;
use crate::motor::{
    MOTOR_CMD_CHANNEL, MOTOR_STATUS, MotorCommand, encoder_a_high, encoder_b_high,
    encoder_invalid_transitions, encoder_transitions, encoder_valid_neg_steps,
    encoder_valid_pos_steps, raw_encoder_count, reset_encoder_transitions, set_raw_encoder_count,
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
            writeln!(
                io,
                "Usage: motor <home|goto|vel|dir|raw|enc|stop|status|pid|autotune|autotune-status>"
            )
            .ok();
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
                writeln!(
                    io,
                    "motor: target velocity set to {:.2}%/s",
                    velocity_pct_per_sec
                )
                .ok();
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
                        writeln!(
                            io,
                            "motor: direction set to reversed (PH low => toward top)"
                        )
                        .ok();
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
                writeln!(
                    io,
                    "logical_encoder_count: {}",
                    status.logical_encoder_count
                )
                .ok();
                writeln!(
                    io,
                    "position_clamped_to_limit: {}",
                    status.position_clamped_to_limit
                )
                .ok();
                writeln!(io, "encoder_a_high: {}", encoder_a_high()).ok();
                writeln!(io, "encoder_b_high: {}", encoder_b_high()).ok();
                writeln!(io, "encoder_transitions: {}", encoder_transitions()).ok();
                writeln!(io, "encoder_valid_pos_steps: {}", encoder_valid_pos_steps()).ok();
                writeln!(io, "encoder_valid_neg_steps: {}", encoder_valid_neg_steps()).ok();
                writeln!(
                    io,
                    "encoder_invalid_transitions: {}",
                    encoder_invalid_transitions()
                )
                .ok();
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
                writeln!(
                    io,
                    "target_velocity: {:.2}%/s",
                    status.target_velocity_pct_per_sec
                )
                .ok();
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
                writeln!(
                    io,
                    "logical_encoder_count: {}",
                    status.logical_encoder_count
                )
                .ok();
                writeln!(io, "raw_encoder_count: {}", status.raw_encoder_count).ok();
                writeln!(
                    io,
                    "position_clamped_to_limit: {}",
                    status.position_clamped_to_limit
                )
                .ok();
                writeln!(io, "counts_per_stroke: {:?}", status.counts_per_stroke).ok();
                writeln!(io, "top_limit: {}", status.at_top_limit).ok();
                writeln!(io, "bottom_limit: {}", status.at_bottom_limit).ok();
                writeln!(io, "raw_mode: {}", status.raw_mode_enabled).ok();
                writeln!(io, "raw_ph_high: {}", status.raw_ph_high).ok();
                writeln!(io, "raw_en_high: {}", status.raw_en_high).ok();
                writeln!(io, "fault: {:?}", status.fault_code).ok();
            }
            "pid" => {
                if args.len() < 3 {
                    writeln!(io, "Usage: motor pid <status|set|load|reset>").ok();
                    return;
                }

                match args[2] {
                    "status" => {
                        let status = *MOTOR_STATUS.lock().await;
                        writeln!(io, "tuning_state: {:?}", status.tuning_state).ok();
                        writeln!(io, "tuning_progress: {:.1}%", status.tuning_progress_pct).ok();
                        writeln!(io, "pid_kp: {:.6}", status.pid_kp).ok();
                        writeln!(io, "pid_ki: {:.6}", status.pid_ki).ok();
                        writeln!(io, "pid_kd: {:.6}", status.pid_kd).ok();
                        writeln!(io, "pid_gains_tuned: {}", status.pid_gains_tuned).ok();
                        writeln!(
                            io,
                            "motor pid: Use 'motor autotune' to calibrate for your motor"
                        )
                        .ok();
                    }
                    "set" => {
                        if args.len() < 6 {
                            writeln!(io, "Usage: motor pid set <kp> <ki> <kd>").ok();
                            return;
                        }

                        let Ok(kp) = args[3].parse::<f32>() else {
                            writeln!(io, "Invalid Kp value").ok();
                            return;
                        };
                        let Ok(ki) = args[4].parse::<f32>() else {
                            writeln!(io, "Invalid Ki value").ok();
                            return;
                        };
                        let Ok(kd) = args[5].parse::<f32>() else {
                            writeln!(io, "Invalid Kd value").ok();
                            return;
                        };

                        MOTOR_CMD_CHANNEL
                            .send(MotorCommand::SetPidGains { kp, ki, kd })
                            .await;
                        writeln!(
                            io,
                            "motor pid: set requested Kp={}, Ki={}, Kd={}",
                            kp, ki, kd
                        )
                        .ok();
                    }
                    "load" => {
                        MOTOR_CMD_CHANNEL.send(MotorCommand::LoadPidGains).await;
                        writeln!(io, "motor pid: reload requested").ok();
                    }
                    "reset" => {
                        MOTOR_CMD_CHANNEL.send(MotorCommand::ResetPidGains).await;
                        writeln!(io, "motor pid: reset requested (defaults restored)").ok();
                    }
                    _ => {
                        writeln!(io, "Usage: motor pid <status|set|load|reset>").ok();
                    }
                }
            }
            "autotune" => {
                // Check if asking for status
                if let Some(&"status") = args.get(2) {
                    let status = *MOTOR_STATUS.lock().await;
                    writeln!(io, "tuning_state: {:?}", status.tuning_state).ok();
                    writeln!(io, "tuning_progress: {:.1}%", status.tuning_progress_pct).ok();
                } else {
                    // Start tuning
                    let status = *MOTOR_STATUS.lock().await;
                    if !status.is_homed {
                        writeln!(io, "motor autotune: Motor must be homed first").ok();
                        return;
                    }

                    writeln!(io, "motor autotune: Starting PID auto-tuning...").ok();
                    writeln!(
                        io,
                        "motor autotune: DO NOT INTERRUPT - Let the motor oscillate for ~2 seconds"
                    )
                    .ok();
                    MOTOR_CMD_CHANNEL.send(MotorCommand::StartTuning).await;
                    writeln!(io, "motor autotune: Tuning started").ok();
                }
            }
            "autotune-status" => {
                let status = *MOTOR_STATUS.lock().await;
                writeln!(io, "tuning_state: {:?}", status.tuning_state).ok();
                writeln!(io, "tuning_progress: {:.1}%", status.tuning_progress_pct).ok();
            }
            _ => {
                writeln!(io, "Usage: motor <home|goto|vel|dir|raw|enc|stop|status|pid|autotune|autotune-status>").ok();
            }
        }
    }
}

pub struct DisplayCommandHandler;

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for DisplayCommandHandler
where
    IO: AsyncWrite + FmtWrite,
{
    async fn execute(&self, args: &[&str], io: &mut IO) {
        if args.len() < 2 {
            writeln!(
                io,
                "Usage: display <power|wifi|play|pause|next|prev|brightness|volume|loop|status|dcim|config|switcher|info>"
            )
            .ok();
            return;
        }

        use crate::display::{
            DISPLAY_CMD_CHANNEL, DISPLAY_STATUS, DisplayCommand, DisplayPlayState,
            DisplayPowerState,
        };

        let send_cmd = |cmd| -> bool { DISPLAY_CMD_CHANNEL.try_send(cmd).is_ok() };

        match args[1] {
            "power" => {
                let Some(sub) = args.get(2) else {
                    writeln!(io, "Usage: display power <on|off>").ok();
                    return;
                };
                match *sub {
                    "on" => {
                        if send_cmd(DisplayCommand::PowerOn) {
                            writeln!(io, "display: power on requested").ok();
                        } else {
                            writeln!(io, "display: error: system busy").ok();
                        }
                    }
                    "off" => {
                        if send_cmd(DisplayCommand::PowerOff) {
                            writeln!(io, "display: power off requested").ok();
                        } else {
                            writeln!(io, "display: error: system busy").ok();
                        }
                    }
                    _ => {
                        writeln!(io, "Usage: display power <on|off>").ok();
                    }
                }
            }
            "wifi" => {
                if args.len() < 3 {
                    writeln!(io, "Usage: display wifi <ssid> [passphrase]").ok();
                    return;
                }
                let mut ssid = heapless::String::new();
                if ssid.push_str(args[2]).is_err() {
                    writeln!(io, "SSID too long").ok();
                    return;
                }
                let mut pass = heapless::String::new();
                if let Some(p) = args.get(3) {
                    if pass.push_str(p).is_err() {
                        writeln!(io, "Passphrase too long").ok();
                        return;
                    }
                }
                if send_cmd(DisplayCommand::SetWifi {
                    ssid,
                    passphrase: pass,
                }) {
                    writeln!(io, "display: wifi credentials configured").ok();
                } else {
                    writeln!(io, "display: error: system busy").ok();
                }
            }
            "play" => {
                let Some(sub) = args.get(2) else {
                    writeln!(io, "Usage: display play <filename>").ok();
                    return;
                };
                let hex_encoded = if let Some(h) = crate::display::encode_filename_to_hex(sub) {
                    h
                } else {
                    writeln!(io, "Failed to encode filename to hex").ok();
                    return;
                };
                if send_cmd(DisplayCommand::PlayFile {
                    filename_hex: hex_encoded,
                }) {
                    writeln!(io, "display: play file requested").ok();
                } else {
                    writeln!(io, "display: error: system busy").ok();
                }
            }
            "pause" => {
                if send_cmd(DisplayCommand::Pause) {
                    writeln!(io, "display: pause requested").ok();
                } else {
                    writeln!(io, "display: error: system busy").ok();
                }
            }
            "next" => {
                if send_cmd(DisplayCommand::Next) {
                    writeln!(io, "display: next file requested").ok();
                } else {
                    writeln!(io, "display: error: system busy").ok();
                }
            }
            "prev" => {
                if send_cmd(DisplayCommand::Previous) {
                    writeln!(io, "display: previous file requested").ok();
                } else {
                    writeln!(io, "display: error: system busy").ok();
                }
            }
            "brightness" => {
                let Some(sub) = args.get(2) else {
                    writeln!(io, "Usage: display brightness <1-3>").ok();
                    return;
                };
                let Ok(b) = sub.parse::<u8>() else {
                    writeln!(io, "Invalid brightness level").ok();
                    return;
                };
                if !(1..=3).contains(&b) {
                    writeln!(io, "Brightness must be between 1 and 3").ok();
                    return;
                }
                if send_cmd(DisplayCommand::SetBrightness(b)) {
                    writeln!(io, "display: brightness set requested").ok();
                } else {
                    writeln!(io, "display: error: system busy").ok();
                }
            }
            "volume" => {
                let Some(sub) = args.get(2) else {
                    writeln!(io, "Usage: display volume <1-3>").ok();
                    return;
                };
                let Ok(v) = sub.parse::<u8>() else {
                    writeln!(io, "Invalid volume level").ok();
                    return;
                };
                if !(1..=3).contains(&v) {
                    writeln!(io, "Volume must be between 1 and 3").ok();
                    return;
                }
                if send_cmd(DisplayCommand::SetVolume(v)) {
                    writeln!(io, "display: volume set requested").ok();
                } else {
                    writeln!(io, "display: error: system busy").ok();
                }
            }
            "loop" => {
                let Some(sub) = args.get(2) else {
                    writeln!(io, "Usage: display loop <one|all>").ok();
                    return;
                };
                let mut mode = heapless::String::new();
                if mode.push_str(sub).is_err() {
                    writeln!(io, "Invalid loop mode value").ok();
                    return;
                }
                if send_cmd(DisplayCommand::SetLoop(mode)) {
                    writeln!(io, "display: loop mode set requested").ok();
                } else {
                    writeln!(io, "display: error: system busy").ok();
                }
            }
            "mute" => {
                if send_cmd(DisplayCommand::SetVolume(1)) {
                    writeln!(io, "display: mute set requested (volume = 1)").ok();
                } else {
                    writeln!(io, "display: error: system busy").ok();
                }
            }
            "unmute" => {
                if send_cmd(DisplayCommand::SetVolume(3)) {
                    writeln!(io, "display: unmute set requested (volume = 3)").ok();
                } else {
                    writeln!(io, "display: error: system busy").ok();
                }
            }
            "dcim" => {
                if send_cmd(DisplayCommand::GetDcim) {
                    writeln!(io, "display: DCIM list request sent").ok();
                    embassy_time::Timer::after_millis(800).await;
                    let status = DISPLAY_STATUS.lock().await;
                    writeln!(io, "Available display files:").ok();
                    for f in &status.files {
                        let decoded = crate::display::decode_hex_filename(f.as_str())
                            .unwrap_or_else(|| {
                                let mut fallback = heapless::String::new();
                                let _ = fallback.push_str("<invalid hex>");
                                fallback
                            });
                        writeln!(io, "  {} ({})", decoded.as_str(), f.as_str()).ok();
                    }
                } else {
                    writeln!(io, "display: error: system busy").ok();
                }
            }
            "config" => {
                if args.len() >= 4 {
                    let mut key = heapless::String::new();
                    let mut value = heapless::String::new();
                    if key.push_str(args[2]).is_err() || value.push_str(args[3]).is_err() {
                        writeln!(io, "display: error: key or value too long").ok();
                        return;
                    }
                    if send_cmd(DisplayCommand::SetConfig { key, value }) {
                        writeln!(io, "display: config set request sent").ok();
                    } else {
                        writeln!(io, "display: error: system busy").ok();
                    }
                } else {
                    if send_cmd(DisplayCommand::GetConfig) {
                        writeln!(io, "display: config request sent").ok();
                        embassy_time::Timer::after_millis(800).await;
                        let status = DISPLAY_STATUS.lock().await.clone();
                        writeln!(io, "Display Settings:").ok();
                        if !status.configs.is_empty() {
                            for entry in &status.configs {
                                writeln!(io, "  {}: {}", entry.key.as_str(), entry.value.as_str())
                                    .ok();
                            }
                        } else {
                            // Fallback if configs haven't been dynamically parsed yet
                            writeln!(io, "  brightness: {}", status.brightness).ok();
                            writeln!(io, "  volume: {}", status.volume).ok();
                            if let Some(l) = status.loop_mode {
                                writeln!(io, "  loop_mode: {}", l.as_str()).ok();
                            } else {
                                writeln!(io, "  loop_mode: unknown").ok();
                            }
                            if let Some(a) = status.angle {
                                writeln!(io, "  angle: {}", a).ok();
                            } else {
                                writeln!(io, "  angle: unknown").ok();
                            }
                            if let Some(b) = status.ble {
                                writeln!(io, "  ble: {}", b.as_str()).ok();
                            } else {
                                writeln!(io, "  ble: unknown").ok();
                            }
                            if let Some(s) = status.switcher {
                                writeln!(io, "  switch: {}", s.as_str()).ok();
                            } else {
                                writeln!(io, "  switch: unknown").ok();
                            }
                            if let Some(s) = status.ssid_config {
                                writeln!(io, "  ssid: {}", s.as_str()).ok();
                            } else {
                                writeln!(io, "  ssid: unknown").ok();
                            }
                        }
                    } else {
                        writeln!(io, "display: error: system busy").ok();
                    }
                }
            }
            "status" => {
                if send_cmd(DisplayCommand::GetStatus) {
                    embassy_time::Timer::after_millis(200).await;
                    let status = DISPLAY_STATUS.lock().await.clone();
                    let p_str = match status.power {
                        DisplayPowerState::Off => "off",
                        DisplayPowerState::PoweringOn => "booting",
                        DisplayPowerState::On => "on",
                        DisplayPowerState::PoweringOff => "shutting down",
                    };
                    writeln!(io, "power state: {}", p_str).ok();
                    if status.power == DisplayPowerState::On {
                        writeln!(
                            io,
                            "station wifi: {}",
                            if status.wifi_connected {
                                "connected"
                            } else {
                                "disconnected"
                            }
                        )
                        .ok();
                        let play_str = match status.play_state {
                            DisplayPlayState::Playing => "playing",
                            DisplayPlayState::Paused => "paused (idle)",
                            DisplayPlayState::Offline => "offline",
                        };
                        writeln!(io, "play state: {}", play_str).ok();
                        if let Some(f) = status.current_file {
                            let decoded = crate::display::decode_hex_filename(f.as_str())
                                .unwrap_or_else(|| {
                                    let mut fallback = heapless::String::new();
                                    let _ = fallback.push_str("<invalid hex>");
                                    fallback
                                });
                            writeln!(io, "current file: {} ({})", decoded.as_str(), f.as_str())
                                .ok();
                            writeln!(io, "progress: {} / {}", status.progress, status.total).ok();
                        }
                    }
                } else {
                    writeln!(io, "display: error: system busy").ok();
                }
            }
            "switcher" => {
                let Some(sub) = args.get(2) else {
                    writeln!(io, "Usage: display switcher <on|off>").ok();
                    return;
                };
                let on = match *sub {
                    "on" => true,
                    "off" => false,
                    _ => {
                        writeln!(io, "Invalid switcher value. Use 'on' or 'off'").ok();
                        return;
                    }
                };
                if send_cmd(DisplayCommand::SetSwitcher(on)) {
                    writeln!(io, "display: switcher set requested").ok();
                } else {
                    writeln!(io, "display: error: system busy").ok();
                }
            }
            "info" => {
                if send_cmd(DisplayCommand::GetInfo) {
                    writeln!(io, "display: info request sent").ok();
                    embassy_time::Timer::after_millis(500).await;
                    let status = DISPLAY_STATUS.lock().await.clone();
                    writeln!(io, "Device Info:").ok();
                    if let Some(m) = status.model {
                        writeln!(io, "  model: {}", m.as_str()).ok();
                    } else {
                        writeln!(io, "  model: unknown").ok();
                    }
                    if let Some(v) = status.sw_version {
                        writeln!(io, "  software version: {}", v.as_str()).ok();
                    } else {
                        writeln!(io, "  software version: unknown").ok();
                    }
                } else {
                    writeln!(io, "display: error: system busy").ok();
                }
            }
            _ => {
                writeln!(
                    io,
                    "Usage: display <power|wifi|play|pause|next|prev|brightness|volume|loop|status|dcim|config|switcher|info>"
                )
                .ok();
            }
        }
    }
}
