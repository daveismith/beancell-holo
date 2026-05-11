#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
// remove the large frames restriction as it interferes with
// embassy.
//#![deny(clippy::large_stack_frames)]

use beancell_holo::cli::handlers::{EchoCommand, MotorCommandHandler, RebootCommand};
use beancell_holo::cli::io::{UartCliIo, UsbCliIo};
use beancell_holo::cli::{Command, CommandDispatcher};
use beancell_holo::motor::encoder::init_encoder_interrupts;
use beancell_holo::motor::task::motor_task;
use beancell_holo::motor::{EncoderConfig, EncoderInputPull, EncoderPins, MotorConfig, MotorPins};
use beancell_holo::wifi::handlers::WifiCommandHandler;
use beancell_holo::wifi::task::{init_wifi, wifi_control_task, wifi_net_task};
use defmt::info;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_hal::Async;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::Pin;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::uart::{Config as UartConfig, Uart, UartRx, UartTx};
use esp_hal::usb_serial_jtag::{UsbSerialJtag, UsbSerialJtagRx, UsbSerialJtagTx};
use panic_rtt_target as _;
use static_cell::StaticCell;

extern crate alloc;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

static RADIO_CTRL: StaticCell<esp_radio::Controller<'static>> = StaticCell::new();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // generator version: 1.2.0

    rtt_target::rtt_init_defmt!();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 66320);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    info!("Embassy initialized!");

    // Initialize USB Serial JTAG
    let (usb_rx, usb_tx) = UsbSerialJtag::new(peripherals.USB_DEVICE)
        .into_async()
        .split();

    let motor_pins = MotorPins {
        en_pin: peripherals.GPIO0.degrade(),
        ph_pin: peripherals.GPIO1.degrade(),
        top_limit_pin: peripherals.GPIO6.degrade(),
        bottom_limit_pin: peripherals.GPIO7.degrade(),
    };
    let encoder_pins = EncoderPins {
        channel_a_pin: peripherals.GPIO21.degrade(),
        channel_b_pin: peripherals.GPIO20.degrade(),
    };
    let encoder_config = EncoderConfig {
        input_pull: EncoderInputPull::None,
        swap_channels: false,
    };

    init_encoder_interrupts(encoder_pins, encoder_config, peripherals.IO_MUX);

    let (uart_rx, uart_tx) = Uart::new(peripherals.UART0, UartConfig::default())
        .unwrap()
        .with_rx(peripherals.GPIO3)
        .with_tx(peripherals.GPIO4)
        .into_async()
        .split();

    let radio_ctrl =
        RADIO_CTRL.init(esp_radio::init().expect("Failed to initialize Wi-Fi/BLE controller"));
    let (wifi_controller, wifi_stack, wifi_runner) =
        init_wifi(radio_ctrl, peripherals.WIFI).expect("Failed to initialize Wi-Fi stack");

    spawner
        .spawn(motor_task(MotorConfig::default(), motor_pins))
        .ok();
    spawner.spawn(wifi_net_task(wifi_runner)).ok();
    spawner
        .spawn(wifi_control_task(wifi_controller, wifi_stack))
        .ok();
    spawner.spawn(usb_cli_task(usb_rx, usb_tx)).ok();
    spawner.spawn(uart_cli_task(uart_rx, uart_tx)).ok();

    loop {
        info!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.0.0/examples
}

#[embassy_executor::task]
async fn usb_cli_task(rx: UsbSerialJtagRx<'static, Async>, tx: UsbSerialJtagTx<'static, Async>) {
    let mut io = UsbCliIo::new(rx, tx);
    let commands: [Command<UsbCliIo<'static>>; 4] = [
        Command::new("echo", "Echo a message back", EchoCommand),
        Command::new(
            "reboot",
            "Reboot device. Usage: reboot [normal|bootloader]",
            RebootCommand,
        ),
        Command::new(
            "motor",
            "Motor control. Usage: motor <home|goto|vel|dir|raw|enc|stop|status>",
            MotorCommandHandler,
        ),
        Command::new("wifi", "Wi-Fi control commands", WifiCommandHandler),
    ];
    let dispatcher = CommandDispatcher::new(&commands);

    beancell_holo::cli::task::run_cli(&dispatcher, &mut io, "beancell> ").await;
}

#[embassy_executor::task]
async fn uart_cli_task(rx: UartRx<'static, Async>, tx: UartTx<'static, Async>) {
    let mut io = UartCliIo::new(rx, tx);
    let commands: [Command<UartCliIo<'static>>; 4] = [
        Command::new("echo", "Echo a message back", EchoCommand),
        Command::new(
            "reboot",
            "Reboot device. Usage: reboot [normal|bootloader]",
            RebootCommand,
        ),
        Command::new(
            "motor",
            "Motor control. Usage: motor <home|goto|vel|dir|raw|enc|stop|status>",
            MotorCommandHandler,
        ),
        Command::new("wifi", "Wi-Fi control commands", WifiCommandHandler),
    ];
    let dispatcher = CommandDispatcher::new(&commands);

    beancell_holo::cli::task::run_cli(&dispatcher, &mut io, "beancell> ").await;
}
