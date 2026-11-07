#![no_std]
#![no_main]

mod ble;
mod board;
mod gallery;
mod jobs;
mod panel;
mod text;

use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Instant, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Flex, Input, InputConfig, Level, Output, OutputConfig, Pull, RtcPinWithResistors};
use esp_hal::rtc_cntl::sleep::{Ext1WakeupSource, WakeupLevel};
use esp_hal::rtc_cntl::Rtc;
use esp_hal::timer::timg::TimerGroup;
use jobs::Job;
use log::{error, info};
use panel::Controller;

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

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

    let mut rail = Output::new(peripherals.GPIO1, Level::High, OutputConfig::default());
    // SD power is active-low. Hold it off until the card driver exists.
    let _sd_off = Output::new(peripherals.GPIO6, Level::High, OutputConfig::default());
    Timer::after(Duration::from_millis(80)).await;

    let pull_up = InputConfig::default().with_pull(Pull::Up);
    let mut rtc = Rtc::new(peripherals.LPWR);
    let mut keys = Keys {
        prev: peripherals.GPIO0,
        next: peripherals.GPIO7,
        left: peripherals.GPIO5,
        right: peripherals.GPIO2,
        confirm: peripherals.GPIO8,
        back: peripherals.GPIO9,
        power: peripherals.GPIO3,
    };

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

    if !uc8279 {
        let prev = Input::new(keys.prev.reborrow(), pull_up);
        let next = Input::new(keys.next.reborrow(), pull_up);
        let left = Input::new(keys.left.reborrow(), pull_up);
        let right = Input::new(keys.right.reborrow(), pull_up);
        let confirm = Input::new(keys.confirm.reborrow(), pull_up);
        let back = Input::new(keys.back.reborrow(), pull_up);
        let power = Input::new(keys.power.reborrow(), pull_up);
        button_loop(prev, next, left, right, confirm, back, power).await;
    }

    // Wi-Fi and MCP stay off on this binary. The controller then gets its
    // default 8 KB task stack, which did not fit beside the Wi-Fi tasks.
    let ble_config = esp_radio::ble::Config::default();
    match esp_radio::ble::controller::BleConnector::new(peripherals.BT, ble_config) {
        Ok(connector) => {
            spawner.spawn(ble::run(connector).unwrap());
        }
        Err(err) => error!("X4 BLE init failed: {err:?}"),
    }

    info!("Hold the power key for 2 seconds to sleep. Any key wakes the reader.");
    jobs::enqueue(
        Job::Frame(text::render("X4\nBLE\nVaultHome X4")),
        "ble",
    )
    .await;

    let mut power_since: Option<Instant> = None;
    loop {
        let prev = Input::new(keys.prev.reborrow(), pull_up);
        let next = Input::new(keys.next.reborrow(), pull_up);
        let left = Input::new(keys.left.reborrow(), pull_up);
        let right = Input::new(keys.right.reborrow(), pull_up);
        let confirm = Input::new(keys.confirm.reborrow(), pull_up);
        let back = Input::new(keys.back.reborrow(), pull_up);
        let power = Input::new(keys.power.reborrow(), pull_up);
        let mut last = [false; 7];
        let sleeping = loop {
            let (page_prev, page_next) =
                poll_buttons(&prev, &next, &left, &right, &confirm, &back, &power, &mut last);
            if power.is_low() {
                let start = *power_since.get_or_insert_with(Instant::now);
                if start.elapsed() >= Duration::from_secs(2) {
                    break true;
                }
            } else {
                power_since = None;
            }
            if page_prev {
                turn_page(&mut epd, -1).await;
            } else if page_next {
                turn_page(&mut epd, 1).await;
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
        };
        if !sleeping {
            continue;
        }
        drop((prev, next, left, right, confirm, back, power));
        sleep_until_any_key(&mut rtc, &mut keys, &mut rail, &mut epd).await;
        power_since = None;
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
) -> (bool, bool) {
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
    let edges = (now[0] && !last[0], now[1] && !last[1]);
    *last = now;
    edges
}

/// Side up is the previous page. Side down is the next page. Either end stays put.
async fn turn_page(epd: &mut panel::Panel<'_>, delta: isize) {
    match gallery::step(delta).await {
        Some(slot) => {
            let which = if delta > 0 { "next" } else { "previous" };
            info!("X4 page {which} -> slot {slot}");
            jobs::set_refreshing(true).await;
            if let Some(frame) = gallery::copy(slot).await {
                epd.refresh_portrait(&frame).await;
            }
            jobs::set_refreshing(false).await;
        }
        None => info!("X4 page step {delta} stayed put"),
    }
}

struct Keys<'d> {
    prev: esp_hal::peripherals::GPIO0<'d>,
    next: esp_hal::peripherals::GPIO7<'d>,
    left: esp_hal::peripherals::GPIO5<'d>,
    right: esp_hal::peripherals::GPIO2<'d>,
    confirm: esp_hal::peripherals::GPIO8<'d>,
    back: esp_hal::peripherals::GPIO9<'d>,
    power: esp_hal::peripherals::GPIO3<'d>,
}

/// Light sleep. PSRAM stays powered, so stored pages are still there on wake.
/// Any of the seven keys wakes the reader. The glass keeps its image.
async fn sleep_until_any_key(
    rtc: &mut Rtc<'_>,
    keys: &mut Keys<'_>,
    rail: &mut Output<'_>,
    epd: &mut panel::Panel<'_>,
) {
    info!("X4 power off. Release the key.");
    show_power_mark(epd, false).await;
    epd.sleep_controller();
    rail.set_low();
    {
        let sense = Input::new(keys.power.reborrow(), InputConfig::default().with_pull(Pull::Up));
        while sense.is_low() {
            Timer::after(Duration::from_millis(20)).await;
        }
    }
    Timer::after(Duration::from_millis(80)).await;

    info!("X4 light sleep. Any key wakes.");
    keys.prev.rtcio_pullup(true);
    keys.next.rtcio_pullup(true);
    keys.left.rtcio_pullup(true);
    keys.right.rtcio_pullup(true);
    keys.confirm.rtcio_pullup(true);
    keys.back.rtcio_pullup(true);
    keys.power.rtcio_pullup(true);
    let mut wake_pins: [&mut dyn esp_hal::gpio::RtcPin; 7] = [
        &mut keys.prev,
        &mut keys.next,
        &mut keys.left,
        &mut keys.right,
        &mut keys.confirm,
        &mut keys.back,
        &mut keys.power,
    ];
    let wake = Ext1WakeupSource::new(&mut wake_pins, WakeupLevel::Low);
    rtc.sleep_light(&[&wake]);
    drop(wake);

    let pull = InputConfig::default().with_pull(Pull::Up);
    let prev = Input::new(keys.prev.reborrow(), pull);
    let next = Input::new(keys.next.reborrow(), pull);
    let left = Input::new(keys.left.reborrow(), pull);
    let right = Input::new(keys.right.reborrow(), pull);
    let confirm = Input::new(keys.confirm.reborrow(), pull);
    let back = Input::new(keys.back.reborrow(), pull);
    let power = Input::new(keys.power.reborrow(), pull);
    while prev.is_low()
        || next.is_low()
        || left.is_low()
        || right.is_low()
        || confirm.is_low()
        || back.is_low()
        || power.is_low()
    {
        Timer::after(Duration::from_millis(20)).await;
    }
    drop((prev, next, left, right, confirm, back, power));
    rail.set_high();
    Timer::after(Duration::from_millis(80)).await;
    info!("X4 power on. Stored pages are still in memory.");
    show_power_mark(epd, true).await;
}

/// Paint a small power mark in the top-right of the page that is already showing.
/// `on` is a filled mark. Off is the open power symbol, and that picture stays
/// on the glass while the reader sleeps.
async fn show_power_mark(epd: &mut panel::Panel<'_>, on: bool) {
    let mut frame = match gallery::info().await.shown {
        Some(slot) => gallery::copy(slot).await.unwrap_or_else(white_frame),
        None => white_frame(),
    };
    stamp_power(&mut frame, on);
    epd.refresh_portrait(&frame).await;
}

fn white_frame() -> alloc::vec::Vec<u8> {
    alloc::vec![0xFFu8; gallery::BYTES]
}

fn stamp_power(frame: &mut [u8], on: bool) {
    let cx = gallery::WIDTH as i32 - 28;
    let cy = 32;
    let radius = 11;
    if on {
        disc(frame, cx, cy, radius, true);
        return;
    }
    disc(frame, cx, cy, radius, false);
    for y in (cy - radius - 6)..=(cy - 2) {
        for x in (cx - 1)..=(cx + 1) {
            ink(frame, x, y);
        }
    }
}

fn disc(frame: &mut [u8], cx: i32, cy: i32, radius: i32, filled: bool) {
    let inner = radius - 2;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let dist = dx * dx + dy * dy;
            let draw = if filled {
                dist <= radius * radius
            } else {
                dist <= radius * radius && dist >= inner * inner
            };
            // Leave a gap at the top of the outline so the stem reads as a power symbol.
            let gap = !filled && dy < -radius / 2 && dx.abs() <= 2;
            if draw && !gap {
                ink(frame, cx + dx, cy + dy);
            }
        }
    }
}

fn ink(frame: &mut [u8], x: i32, y: i32) {
    if x < 0 || y < 0 || x >= gallery::WIDTH as i32 || y >= gallery::HEIGHT as i32 {
        return;
    }
    let x = x as usize;
    let y = y as usize;
    let index = y * gallery::STRIDE + x / 8;
    frame[index] &= !(0x80 >> (x % 8));
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
