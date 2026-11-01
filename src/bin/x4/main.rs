#![no_std]
#![no_main]

mod board;
mod font;
mod gallery;
mod jobs;
mod mcp;
mod net;
mod panel;
mod text;

use embassy_futures::select::{Either, select};
use embassy_net::{Config, StackResources};
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Flex, Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;
use jobs::Job;
use log::{error, info};
use panel::Controller;
use static_cell::StaticCell;

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

static STACK_RESOURCES: StaticCell<StackResources<4>> = StaticCell::new();

#[esp_rtos::main]
async fn main(spawner: embassy_executor::Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    let psram_config = esp_hal::psram::PsramConfig {
        mode: esp_hal::psram::PsramMode::OctalSpi,
        ..Default::default()
    };
    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram, psram_config);
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
    gallery::reserve().await;

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    info!(
        "VaultHomeDisp X4 {}x{} portrait",
        board::VIEW_WIDTH,
        board::VIEW_HEIGHT
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

    let mut mosi = Flex::new(peripherals.GPIO11);
    mosi.apply_output_config(&OutputConfig::default());
    mosi.set_low();
    mosi.set_output_enable(true);

    let mut epd = panel::Panel::new(
        Output::new(peripherals.GPIO12, Level::Low, OutputConfig::default()),
        mosi,
        Output::new(peripherals.GPIO13, Level::High, OutputConfig::default()),
        Output::new(peripherals.GPIO14, Level::Low, OutputConfig::default()),
        Output::new(peripherals.GPIO10, Level::High, OutputConfig::default()),
        Input::new(
            peripherals.GPIO18,
            InputConfig::default().with_pull(Pull::None),
        ),
    );

    let controller = epd.probe().await;
    let uc8279 = match controller {
        Controller::Uc8179 => {
            info!("X4 controller UC8179 — no waveform for this part yet");
            false
        }
        Controller::Uc8279 => {
            info!("X4 controller UC8279");
            true
        }
        Controller::Unknown(id) => {
            info!("X4 controller unknown {id:02x?} — no waveform sent");
            false
        }
    };

    if !uc8279 || !net::ssid_ready() {
        if uc8279 && !net::ssid_ready() {
            error!("WIFI_SSID is empty. Rebuild with wifi.env");
            epd.show_uc8279_mark().await;
        }
        button_loop(prev, next, left, right, confirm, back, power).await;
    }

    let rng = Rng::new();
    let r1 = rng.random();
    let r2 = rng.random();
    mcp::seed_session(r1 ^ r2);
    let seed = (r1 as u64) << 32 | r2 as u64;

    let (wifi_controller, interfaces) = esp_radio::wifi::new(peripherals.WIFI, Default::default())
        .expect("Failed to initialize Wi-Fi controller");
    let (stack, runner) = embassy_net::new(
        interfaces.station,
        Config::dhcpv4(Default::default()),
        STACK_RESOURCES.init(StackResources::new()),
        seed,
    );
    spawner.spawn(net::net_task(runner).unwrap());
    spawner.spawn(net::wifi_task(wifi_controller).unwrap());
    spawner.spawn(mcp::run(stack).unwrap());

    if net::fallback_ssid().is_empty() {
        info!("Connecting to SSID `{}` ...", net::primary_ssid());
    } else {
        info!(
            "Connecting to SSID `{}` (fallback `{}`)",
            net::primary_ssid(),
            net::fallback_ssid()
        );
    }

    let mut announced = false;
    let mut last = [false; 7];
    loop {
        poll_buttons(&prev, &next, &left, &right, &confirm, &back, &power, &mut last);
        if !announced {
            if let Some(ip) = net::ipv4(stack) {
                jobs::set_ip(ip.clone()).await;
                let url = alloc::format!("http://{ip}/mcp");
                info!("MCP: POST {url}  tools: show_text, show_bitmap, panel_status");
                let page = text::render(&alloc::format!("X4\n{url}"));
                jobs::enqueue(Job::Frame(page), &url).await;
                announced = true;
            }
        }
        while let Some(job) = jobs::take().await {
            jobs::set_refreshing(true).await;
            match job {
                Job::Frame(frame) => epd.refresh_portrait(&frame).await,
                Job::Slot(slot) => {
                    info!("X4 show stored slot {slot}");
                    if let Some(frame) = gallery::copy(slot).await {
                        epd.refresh_portrait(&frame).await;
                    } else {
                        info!("X4 slot {slot} is empty");
                    }
                }
            }
            jobs::set_refreshing(false).await;
        }
        match select(jobs::wait(), Timer::after(Duration::from_millis(20))).await {
            Either::First(()) | Either::Second(()) => {}
        }
    }
}

fn poll_buttons(
    prev: &Input<'_>,
    next: &Input<'_>,
    left: &Input<'_>,
    right: &Input<'_>,
    confirm: &Input<'_>,
    back: &Input<'_>,
    power: &Input<'_>,
    last: &mut [bool; 7],
) {
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
    *last = now;
}

async fn button_loop(
    prev: Input<'_>,
    next: Input<'_>,
    left: Input<'_>,
    right: Input<'_>,
    confirm: Input<'_>,
    back: Input<'_>,
    power: Input<'_>,
) -> ! {
    let mut last = [false; 7];
    loop {
        poll_buttons(&prev, &next, &left, &right, &confirm, &back, &power, &mut last);
        Timer::after(Duration::from_millis(20)).await;
    }
}
