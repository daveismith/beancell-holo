use portable_atomic::{AtomicU8, Ordering};

use esp_hal::{
    gpio::{Event, Input, InputConfig, Io, Pull},
    handler,
    peripherals::IO_MUX,
};

use super::{
    EncoderConfig, EncoderInputPull, EncoderPins, MOTOR_ENCODER_A_LEVEL, MOTOR_ENCODER_B_LEVEL,
    MOTOR_ENCODER_INVALID_TRANSITIONS, MOTOR_ENCODER_TRANSITIONS, MOTOR_ENCODER_VALID_NEG_STEPS,
    MOTOR_ENCODER_VALID_POS_STEPS, MOTOR_RAW_ENCODER_COUNT,
};

// Safety: written exactly once during init (before any interrupt can fire),
// then accessed only from the ISR. ESP32-C3 is single-core; no concurrent
// access is possible between the ISR and any task.
static mut ENCODER_A: Option<Input<'static>> = None;
static mut ENCODER_B: Option<Input<'static>> = None;

// 2-bit combined previous quadrature state: (A_high << 1) | B_high
static PREV_STATE: AtomicU8 = AtomicU8::new(0);
static SWAP_CHANNELS: AtomicU8 = AtomicU8::new(0);

// Flat X4 quadrature decode table. Index = (prev << 2) | curr.
// Forward sequence (A leads B): state 0 → 2 → 3 → 1 → 0, each step = +1.
// Diagonal transitions (two states skipped) are ambiguous; treated as 0.
#[rustfmt::skip]
const DELTA: [i8; 16] = [
//  curr→  0   1   2   3
           0, -1,  1,  0,  // prev=0
           1,  0,  0, -1,  // prev=1
          -1,  0,  0,  1,  // prev=2
           0,  1, -1,  0,  // prev=3
];

#[handler]
fn gpio_encoder_handler() {
    // SAFETY: see comment on ENCODER_A/B statics above.
    let (a, b) = unsafe {
        match (
            (*(&raw mut ENCODER_A)).as_mut(),
            (*(&raw mut ENCODER_B)).as_mut(),
        ) {
            (Some(a), Some(b)) => (a, b),
            _ => return,
        }
    };

    let a_fired = a.is_interrupt_set();
    let b_fired = b.is_interrupt_set();

    if !a_fired && !b_fired {
        return;
    }

    if a_fired {
        a.clear_interrupt();
    }
    if b_fired {
        b.clear_interrupt();
    }

    let a_high = a.is_high();
    let b_high = b.is_high();
    let swap = SWAP_CHANNELS.load(Ordering::Relaxed) != 0;

    let (logical_a_high, logical_b_high) = if swap {
        (b_high, a_high)
    } else {
        (a_high, b_high)
    };

    let curr = ((logical_a_high as u8) << 1) | (logical_b_high as u8);
    let prev = PREV_STATE.swap(curr, Ordering::Relaxed);
    let delta = DELTA[((prev << 2) | curr) as usize];

    MOTOR_ENCODER_TRANSITIONS.fetch_add(1, Ordering::Relaxed);
    match delta {
        1 => {
            MOTOR_RAW_ENCODER_COUNT.fetch_add(1, Ordering::Relaxed);
            MOTOR_ENCODER_VALID_POS_STEPS.fetch_add(1, Ordering::Relaxed);
        }
        -1 => {
            MOTOR_RAW_ENCODER_COUNT.fetch_add(-1, Ordering::Relaxed);
            MOTOR_ENCODER_VALID_NEG_STEPS.fetch_add(1, Ordering::Relaxed);
        }
        _ => {
            if prev != curr {
                MOTOR_ENCODER_INVALID_TRANSITIONS.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    MOTOR_ENCODER_A_LEVEL.store(logical_a_high as u8, Ordering::Relaxed);
    MOTOR_ENCODER_B_LEVEL.store(logical_b_high as u8, Ordering::Relaxed);
}

pub fn init_encoder_interrupts(pins: EncoderPins, config: EncoderConfig, io_mux: IO_MUX<'static>) {
    let mut io = Io::new(io_mux);
    io.set_interrupt_handler(gpio_encoder_handler);

    let input_config = match config.input_pull {
        EncoderInputPull::None => InputConfig::default(),
        EncoderInputPull::Up => InputConfig::default().with_pull(Pull::Up),
        EncoderInputPull::Down => InputConfig::default().with_pull(Pull::Down),
    };
    let mut ch_a = Input::new(pins.channel_a_pin, input_config);
    let mut ch_b = Input::new(pins.channel_b_pin, input_config);

    let a_high = ch_a.is_high();
    let b_high = ch_b.is_high();
    let (logical_a_high, logical_b_high) = if config.swap_channels {
        (b_high, a_high)
    } else {
        (a_high, b_high)
    };

    ch_a.listen(Event::AnyEdge);
    ch_b.listen(Event::AnyEdge);

    // Initialise all shared state before placing pins in statics, after which
    // the ISR may fire at any time.
    MOTOR_RAW_ENCODER_COUNT.store(0, Ordering::Relaxed);
    MOTOR_ENCODER_TRANSITIONS.store(0, Ordering::Relaxed);
    MOTOR_ENCODER_VALID_POS_STEPS.store(0, Ordering::Relaxed);
    MOTOR_ENCODER_VALID_NEG_STEPS.store(0, Ordering::Relaxed);
    MOTOR_ENCODER_INVALID_TRANSITIONS.store(0, Ordering::Relaxed);
    MOTOR_ENCODER_A_LEVEL.store(logical_a_high as u8, Ordering::Relaxed);
    MOTOR_ENCODER_B_LEVEL.store(logical_b_high as u8, Ordering::Relaxed);
    PREV_STATE.store(
        ((logical_a_high as u8) << 1) | (logical_b_high as u8),
        Ordering::Relaxed,
    );
    SWAP_CHANNELS.store(config.swap_channels as u8, Ordering::Relaxed);

    // SAFETY: listen() has been called above; on a single-core device the ISR
    // cannot fire until this function returns and the CPU re-enables interrupts.
    unsafe {
        ENCODER_A = Some(ch_a);
        ENCODER_B = Some(ch_b);
    }
}
