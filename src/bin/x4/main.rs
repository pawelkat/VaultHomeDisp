#![no_std]
#![no_main]

mod board;
mod panel;

use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::spi::Mode;
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use log::info;
use panel::Controller;

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(_spawner: embassy_executor::Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    let psram_config = esp_hal::psram::PsramConfig {
        mode: esp_hal::psram::PsramMode::OctalSpi,
        ..Default::default()
    };
    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram, psram_config);
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    info!(
        "VaultHomeDisp X4 bring-up {}x{} native",
        board::NATIVE_WIDTH,
        board::NATIVE_HEIGHT
    );

    let _rail = Output::new(peripherals.GPIO1, Level::High, OutputConfig::default());
    // SD power is active-low. Hold it off until the card driver exists.
    let _sd_off = Output::new(peripherals.GPIO6, Level::High, OutputConfig::default());
    Timer::after(Duration::from_millis(80)).await;

    let pull_up = InputConfig::default().with_pull(Pull::Up);
    let prev = Input::new(peripherals.GPIO0, pull_up);
    let next = Input::new(peripherals.GPIO7, pull_up);
    let left = Input::new(peripherals.GPIO5, pull_up);
    let right = Input::new(peripherals.GPIO2, pull_up);
    let confirm = Input::new(peripherals.GPIO8, pull_up);
    let back = Input::new(peripherals.GPIO9, pull_up);
    let power = Input::new(peripherals.GPIO3, pull_up);

    let spi = Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(4))
            .with_mode(Mode::_0),
    )
    .unwrap()
    .with_sck(peripherals.GPIO12)
    .with_mosi(peripherals.GPIO11);

    let mut epd = panel::Panel::new(
        spi,
        Output::new(peripherals.GPIO13, Level::High, OutputConfig::default()),
        Output::new(peripherals.GPIO14, Level::Low, OutputConfig::default()),
        Output::new(peripherals.GPIO10, Level::High, OutputConfig::default()),
        Input::new(
            peripherals.GPIO18,
            InputConfig::default().with_pull(Pull::Down),
        ),
    );

    match epd.probe().await {
        Controller::Uc8179 => info!("X4 controller UC8179"),
        Controller::Uc8279 => info!("X4 controller UC8279"),
        Controller::Unknown(id) => info!("X4 controller unknown {id:02x?} — no waveform sent"),
    }

    let mut last = [false; 7];
    loop {
        let now = [
            prev.is_low(),
            next.is_low(),
            left.is_low(),
            right.is_low(),
            confirm.is_low(),
            back.is_low(),
            power.is_low(),
        ];
        const NAMES: [&str; 7] = ["PREV", "NEXT", "LEFT", "RIGHT", "CONFIRM", "BACK", "POWER"];
        for i in 0..7 {
            if now[i] && !last[i] {
                info!("{} pressed", NAMES[i]);
            }
        }
        last = now;
        Timer::after(Duration::from_millis(20)).await;
    }
}
