#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

mod board;
mod epd;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use epd::Epd;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::spi::Mode;
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use log::info;
use slint::platform::software_renderer::{
    LineBufferProvider, MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel,
};
use slint::platform::{Platform, WindowAdapter};
use slint::PhysicalSize;

extern crate alloc;

slint::include_modules!();

esp_bootloader_esp_idf::esp_app_desc!();

struct EspPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for EspPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> core::time::Duration {
        core::time::Duration::from_millis(embassy_time::Instant::now().as_millis())
    }

    fn debug_log(&self, arguments: core::fmt::Arguments) {
        log::debug!("[slint] {arguments}");
    }
}

struct PackedLines<'a> {
    packed: &'a mut [u8],
    line: &'a mut [Rgb565Pixel],
}

impl LineBufferProvider for PackedLines<'_> {
    type TargetPixel = Rgb565Pixel;

    fn process_line(
        &mut self,
        line: usize,
        range: core::ops::Range<usize>,
        render_fn: impl FnOnce(&mut [Self::TargetPixel]),
    ) {
        let slice = &mut self.line[range.clone()];
        render_fn(slice);
        for (dx, pixel) in slice.iter().enumerate() {
            let x = range.start + dx;
            let pos = (line * board::WIDTH + x) / 8;
            let bit = 7 - (x % 8);
            if rgb565_is_white(*pixel) {
                self.packed[pos] |= 1 << bit;
            } else {
                self.packed[pos] &= !(1 << bit);
            }
        }
    }
}

fn rgb565_is_white(pixel: Rgb565Pixel) -> bool {
    let v = pixel.0;
    let r = (v >> 11) << 3;
    let g = ((v >> 5) & 0x3F) << 2;
    let b = (v & 0x1F) << 3;
    u16::from(r) + u16::from(g) + u16::from(b) >= 382
}

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // generator version: 1.3.0
    // generator parameters: --chip esp32s3 -o embassy -o unstable-hal -o log -o esp-backtrace -o alloc

    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);

    let psram_config = esp_hal::psram::PsramConfig {
        mode: esp_hal::psram::PsramMode::OctalSpi,
        ..Default::default()
    };
    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram, psram_config);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    info!(
        "crowpanel-embassy on {}x{} dual SSD1683",
        board::WIDTH,
        board::HEIGHT
    );
    info!("Embassy initialized!");

    let _epd_pwr = Output::new(peripherals.GPIO7, Level::High, OutputConfig::default());
    Timer::after(Duration::from_millis(100)).await;

    let led = Output::new(peripherals.GPIO41, Level::Low, OutputConfig::default());

    let pull_up = InputConfig::default().with_pull(Pull::Up);
    let exit = Input::new(peripherals.GPIO1, pull_up);
    let menu = Input::new(peripherals.GPIO2, pull_up);
    let dial_down = Input::new(peripherals.GPIO4, pull_up);
    let dial_push = Input::new(peripherals.GPIO5, pull_up);
    let dial_up = Input::new(peripherals.GPIO6, pull_up);

    let spi = Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(2))
            .with_mode(Mode::_0),
    )
    .unwrap()
    .with_sck(peripherals.GPIO12)
    .with_mosi(peripherals.GPIO11);

    let mut epd = Epd::new(
        spi,
        Output::new(peripherals.GPIO45, Level::High, OutputConfig::default()),
        Output::new(peripherals.GPIO46, Level::High, OutputConfig::default()),
        Output::new(peripherals.GPIO47, Level::High, OutputConfig::default()),
        Input::new(
            peripherals.GPIO48,
            InputConfig::default().with_pull(Pull::Down),
        ),
    );
    epd.init().await;

    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(EspPlatform {
        window: window.clone(),
    }))
    .unwrap();
    window.set_size(PhysicalSize::new(
        board::WIDTH as u32,
        board::HEIGHT as u32,
    ));

    let ui = MainWindow::new().unwrap();
    ui.show().unwrap();
    slint::platform::update_timers_and_animations();

    let mut packed = vec![0xFFu8; epd::BUFFER_LEN];
    let mut line: Vec<Rgb565Pixel> = vec![Rgb565Pixel(0); board::WIDTH];
    let drawn = window.draw_if_needed(|renderer| {
        renderer.render_by_line(PackedLines {
            packed: &mut packed,
            line: &mut line,
        });
    });
    info!("slint render dirty={drawn}");

    epd.display(&packed).await;
    epd.deep_sleep().await;
    info!("hi Pawel is on the panel");

    spawner.spawn(led_task(led).unwrap());

    let mut last = [false; 5];
    loop {
        let now = [
            menu.is_low(),
            exit.is_low(),
            dial_up.is_low(),
            dial_down.is_low(),
            dial_push.is_low(),
        ];
        const NAMES: [&str; 5] = ["MENU", "EXIT", "DIAL_UP", "DIAL_DOWN", "DIAL_PUSH"];
        for i in 0..5 {
            if now[i] && !last[i] {
                info!("{} pressed", NAMES[i]);
            }
        }
        last = now;
        Timer::after(Duration::from_millis(20)).await;
    }
}

#[embassy_executor::task]
async fn led_task(mut led: Output<'static>) {
    loop {
        led.toggle();
        Timer::after(Duration::from_millis(500)).await;
    }
}
