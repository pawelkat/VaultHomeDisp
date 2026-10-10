#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]
#![allow(non_snake_case, reason = "the project name is VaultHomeDisp")]

mod board;
mod epd;
mod gallery;
mod mcp;
mod panel;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU8, Ordering};
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_net::{Config, Runner, Stack, StackResources};
use embassy_time::{Duration, Timer};
use epd::Epd;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::rng::Rng;
use esp_hal::spi::Mode;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use log::{error, info, warn};
use panel::PanelJob;
use slint::PhysicalSize;
use slint::platform::software_renderer::{
    LineBufferProvider, MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel,
};
use slint::platform::{Platform, WindowAdapter};
use static_cell::StaticCell;

extern crate alloc;

slint::include_modules!();

const WIFI_SSID: &str = env!("WIFI_SSID");
const WIFI_PASSWORD: &str = env!("WIFI_PASSWORD");
const WIFI_SSID2: &str = env!("WIFI_SSID2");
const WIFI_PASSWORD2: &str = env!("WIFI_PASSWORD2");
/// 0 = primary, 1 = optional fallback.
static WIFI_NET: AtomicU8 = AtomicU8::new(0);

static STACK_RESOURCES: StaticCell<StackResources<4>> = StaticCell::new();

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
            // 180°: USB-C up, buttons on the left.
            let rx = board::WIDTH - 1 - x;
            let ry = board::HEIGHT - 1 - line;
            let pos = (ry * board::WIDTH + rx) / 8;
            let bit = 7 - (rx % 8);
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
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // PSRAM first so ordinary allocations stay off the internal heap Wi-Fi needs.
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
        "VaultHomeDisp on {}x{} dual SSD1683 (USB-C up, buttons left)",
        board::WIDTH,
        board::HEIGHT
    );

    let _epd_pwr = Output::new(peripherals.GPIO7, Level::High, OutputConfig::default());
    Timer::after(Duration::from_millis(100)).await;

    let led = Output::new(peripherals.GPIO41, Level::Low, OutputConfig::default());
    spawner.spawn(led_task(led).unwrap());

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

    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(EspPlatform {
        window: window.clone(),
    }))
    .unwrap();
    window.set_size(PhysicalSize::new(board::WIDTH as u32, board::HEIGHT as u32));
    let ui = MainWindow::new().unwrap();
    ui.show().unwrap();

    let mut packed = vec![0xFFu8; epd::BUFFER_LEN];
    let mut line: Vec<Rgb565Pixel> = vec![Rgb565Pixel(0); board::WIDTH];

    if WIFI_SSID.is_empty() {
        error!("WIFI_SSID is empty. Rebuild with wifi.env or WIFI_SSID / WIFI_PASSWORD");
        epd.init().await;
        paint(
            &ui,
            &window,
            &mut epd,
            &mut packed,
            &mut line,
            "Set WIFI_SSID",
            "wifi.env",
            None,
        )
        .await;
        button_loop(menu, exit, dial_up, dial_down, dial_push).await;
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
    spawner.spawn(net_task(runner).unwrap());
    spawner.spawn(wifi_task(wifi_controller).unwrap());

    if WIFI_SSID2.is_empty() {
        info!("Connecting to SSID `{WIFI_SSID}` ...");
    } else {
        info!("Connecting to SSID `{WIFI_SSID}` (fallback `{WIFI_SSID2}`)");
    }

    // Panel init overlaps association. The controller needs a full refresh
    // before the first image.
    epd.init().await;

    loop {
        if let Some(ip) = ipv4(stack) {
            panel::set_ip(ip).await;
            break;
        }
        Timer::after(Duration::from_millis(200)).await;
    }
    let ip = panel::ip().await;
    let url = alloc::format!("http://{ip}/mcp");
    info!(
        "MCP: POST {url}  tools: show_text, show_bitmap, panel_status  bitmaps {}x{} x{}",
        gallery::WIDTH,
        gallery::HEIGHT,
        gallery::CAPACITY
    );

    spawner.spawn(mcp::run(stack).unwrap());
    spawner.spawn(button_task(menu, exit, dial_up, dial_down, dial_push).unwrap());
    spawner.spawn(auto_page_task().unwrap());

    panel::set_refreshing(true).await;
    paint(
        &ui,
        &window,
        &mut epd,
        &mut packed,
        &mut line,
        "MCP ready",
        &url,
        None,
    )
    .await;
    panel::note_shown("MCP ready").await;

    loop {
        panel::wait_kick().await;
        while let Some(job) = panel::take().await {
            match job {
                PanelJob::Text(text) => {
                    info!("panel refresh ({} chars)", text.chars().count());
                    panel::set_refreshing(true).await;
                    paint(
                        &ui,
                        &window,
                        &mut epd,
                        &mut packed,
                        &mut line,
                        &text,
                        &url,
                        None,
                    )
                    .await;
                    panel::note_shown(&text).await;
                    info!("panel refresh done");
                }
                PanelJob::Bitmap(index) => {
                    let Some(bitmap) = gallery::copy(index).await else {
                        warn!("bitmap slot {index} is empty");
                        continue;
                    };
                    let count = gallery::info().await.count.max(1);
                    let status = alloc::format!("{}/{}  {url}", index + 1, count);
                    info!("panel refresh bitmap slot {index}");
                    panel::set_refreshing(true).await;
                    paint(
                        &ui,
                        &window,
                        &mut epd,
                        &mut packed,
                        &mut line,
                        "",
                        &status,
                        Some(&bitmap),
                    )
                    .await;
                    panel::note_shown(&alloc::format!("bitmap {index}")).await;
                    info!("panel refresh done");
                }
            }
        }
    }
}

async fn paint(
    ui: &MainWindow,
    window: &MinimalSoftwareWindow,
    epd: &mut Epd<'_>,
    packed: &mut [u8],
    line: &mut [Rgb565Pixel],
    message: &str,
    status: &str,
    bitmap: Option<&[u8]>,
) {
    packed.fill(0xFF);
    ui.set_message(message.into());
    ui.set_status(status.into());
    window.request_redraw();
    slint::platform::update_timers_and_animations();
    let drawn = window.draw_if_needed(|renderer| {
        renderer.render_by_line(PackedLines { packed, line });
    });
    if let Some(bitmap) = bitmap {
        blit_bitmap(packed, bitmap);
    }
    info!("slint render dirty={drawn}");
    epd.display(packed).await;
}

/// Paint a content-area bitmap in logical coordinates (USB-C up).
/// Bit 1 is white, MSB is the left pixel. The status strip is left alone.
fn blit_bitmap(packed: &mut [u8], bitmap: &[u8]) {
    for y in 0..gallery::HEIGHT {
        for x in 0..gallery::WIDTH {
            let src = bitmap[y * gallery::STRIDE + x / 8];
            let white = src & (1 << (7 - (x % 8))) != 0;
            let rx = board::WIDTH - 1 - x;
            let ry = board::HEIGHT - 1 - y;
            let pos = (ry * board::WIDTH + rx) / 8;
            let bit = 7 - (rx % 8);
            if white {
                packed[pos] |= 1 << bit;
            } else {
                packed[pos] &= !(1 << bit);
            }
        }
    }
}

#[embassy_executor::task]
async fn button_task(
    menu: Input<'static>,
    exit: Input<'static>,
    dial_up: Input<'static>,
    dial_down: Input<'static>,
    dial_push: Input<'static>,
) {
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
        // Silkscreen up points at the visual bottom (USB-C is up).
        if now[2] && !last[2] {
            show_step(1).await;
        }
        if now[3] && !last[3] {
            show_step(-1).await;
        }
        last = now;
        Timer::after(Duration::from_millis(20)).await;
    }
}

#[embassy_executor::task]
async fn auto_page_task() {
    loop {
        let start = panel::epoch();
        Timer::after(Duration::from_secs(gallery::AUTO_PAGE_SECS)).await;
        if panel::epoch() != start {
            continue;
        }
        if let Some(index) = gallery::advance().await {
            info!("auto page -> slot {index}");
            panel::enqueue(panel::PanelJob::Bitmap(index)).await;
        }
    }
}

async fn show_step(delta: isize) {
    match gallery::step(delta).await {
        Some(index) => {
            let which = if delta > 0 { "next" } else { "previous" };
            info!("bitmap {which} -> slot {index}");
            panel::enqueue(panel::PanelJob::Bitmap(index)).await;
        }
        None => info!("bitmap step {delta} stayed put"),
    }
}

fn poll_buttons(
    menu: &Input<'_>,
    exit: &Input<'_>,
    dial_up: &Input<'_>,
    dial_down: &Input<'_>,
    dial_push: &Input<'_>,
    last: &mut [bool; 5],
) {
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
    *last = now;
}

async fn button_loop(
    menu: Input<'_>,
    exit: Input<'_>,
    dial_up: Input<'_>,
    dial_down: Input<'_>,
    dial_push: Input<'_>,
) -> ! {
    let mut last = [false; 5];
    loop {
        poll_buttons(&menu, &exit, &dial_up, &dial_down, &dial_push, &mut last);
        Timer::after(Duration::from_millis(20)).await;
    }
}

fn ipv4(stack: Stack<'_>) -> Option<String> {
    let cfg = stack.config_v4()?;
    let cidr = cfg.address.to_string();
    Some(cidr.split('/').next().unwrap_or(cidr.as_str()).to_string())
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, esp_radio::wifi::Interface<'static>>) -> ! {
    runner.run().await
}

fn wifi_networks() -> [(&'static str, &'static str); 2] {
    [(WIFI_SSID, WIFI_PASSWORD), (WIFI_SSID2, WIFI_PASSWORD2)]
}

#[embassy_executor::task]
async fn wifi_task(mut controller: esp_radio::wifi::WifiController<'static>) {
    let nets = wifi_networks();
    let mut idx = 0usize;
    loop {
        let Some(j) = (0..nets.len())
            .map(|k| (idx + k) % nets.len())
            .find(|&n| !nets[n].0.is_empty())
        else {
            error!("No Wi-Fi SSID configured");
            return;
        };
        let (ssid, password) = nets[j];
        WIFI_NET.store(j as u8, Ordering::Relaxed);

        let station_config = esp_radio::wifi::sta::StationConfig::default()
            .with_ssid(ssid)
            .with_password(password.to_string());
        let radio_config = esp_radio::wifi::Config::Station(station_config);
        if let Err(e) = controller.set_config(&radio_config) {
            error!("Wi-Fi set_config `{ssid}` failed: {e:?}");
            idx = j.wrapping_add(1);
            Timer::after(Duration::from_secs(2)).await;
            continue;
        }

        let kind = if j == 0 { "primary" } else { "fallback" };
        info!("Connecting to `{ssid}` ({kind})");
        match select(
            controller.connect_async(),
            Timer::after(Duration::from_secs(15)),
        )
        .await
        {
            Either::First(Ok(_info)) => {
                info!("Wi-Fi associated with `{ssid}`");
                let _ = controller.wait_for_disconnect_async().await;
                warn!("Wi-Fi disconnected from `{ssid}`, retrying primary...");
                idx = 0;
                Timer::after(Duration::from_secs(2)).await;
            }
            Either::First(Err(e)) => {
                error!("Wi-Fi connect `{ssid}` failed: {e:?}");
                idx = j.wrapping_add(1);
                let wait = if nets.iter().filter(|(s, _)| !s.is_empty()).count() > 1 {
                    2
                } else {
                    5
                };
                Timer::after(Duration::from_secs(wait)).await;
            }
            Either::Second(()) => {
                warn!("Wi-Fi connect `{ssid}` timed out");
                let _ = controller.disconnect_async().await;
                idx = j.wrapping_add(1);
                Timer::after(Duration::from_secs(1)).await;
            }
        }
    }
}

#[embassy_executor::task]
async fn led_task(mut led: Output<'static>) {
    loop {
        led.toggle();
        Timer::after(Duration::from_millis(500)).await;
    }
}
