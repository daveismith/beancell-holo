use embassy_futures::select::{Either, select};
use esp_hal::gpio::{Input, InputConfig, Pull};

use super::{EncoderPins, add_encoder_count, set_encoder_count};

fn state(a: bool, b: bool) -> u8 {
    ((a as u8) << 1) | (b as u8)
}

fn quadrature_delta(prev: u8, next: u8) -> i32 {
    match (prev, next) {
        (0b00, 0b01) | (0b01, 0b11) | (0b11, 0b10) | (0b10, 0b00) => 1,
        (0b00, 0b10) | (0b10, 0b11) | (0b11, 0b01) | (0b01, 0b00) => -1,
        _ => 0,
    }
}

#[embassy_executor::task]
pub async fn encoder_task(pins: EncoderPins) {
    let cfg = InputConfig::default().with_pull(Pull::Up);
    let mut ch_a = Input::new(pins.channel_a_pin, cfg);
    let mut ch_b = Input::new(pins.channel_b_pin, cfg);

    let mut prev = state(ch_a.is_high(), ch_b.is_high());
    set_encoder_count(0);

    loop {
        match select(ch_a.wait_for_any_edge(), ch_b.wait_for_any_edge()).await {
            Either::First(_) | Either::Second(_) => {
                let next = state(ch_a.is_high(), ch_b.is_high());
                let delta = quadrature_delta(prev, next);
                prev = next;
                if delta != 0 {
                    add_encoder_count(delta);
                }
            }
        }
    }
}
