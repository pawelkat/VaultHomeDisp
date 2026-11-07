//! Half-duplex controller probe. No waveform is sent.
//!
//! The UC81xx answers command 0x70 on the MOSI pin. FreeInk `XteinkDetect`
//! clocks that byte with GPIO, releases MOSI, and samples while SCLK is low
//! because the controller shifts on the falling edge. A hardware SPI transfer
//! cannot do that: this bus has no MISO, and DC has to change between the
//! command and the reply.

use embassy_time::{Duration, Instant, Timer};
use esp_hal::delay::Delay;
use esp_hal::gpio::{Flex, Input, InputConfig, Output, OutputConfig, Pull};
use log::info;

pub struct Panel<'d> {
    sclk: Output<'d>,
    mosi: Flex<'d>,
    cs: Output<'d>,
    dc: Output<'d>,
    rst: Output<'d>,
    busy: Input<'d>,
}

pub enum Controller {
    Uc8179,
    Uc8279,
    Unknown([u8; 5]),
}

struct Pass {
    ver: [u8; 5],
    flg: u8,
    busy_high: bool,
}

impl<'d> Panel<'d> {
    pub fn new(
        sclk: Output<'d>,
        mosi: Flex<'d>,
        cs: Output<'d>,
        dc: Output<'d>,
        rst: Output<'d>,
        busy: Input<'d>,
    ) -> Self {
        let mut panel = Self {
            sclk,
            mosi,
            cs,
            dc,
            rst,
            busy,
        };
        panel.mosi.apply_output_config(&OutputConfig::default());
        panel.mosi_output();
        panel.sclk.set_low();
        panel.cs.set_high();
        panel.dc.set_low();
        panel
    }

    /// Two resets (1 ms, then the vendor 50 ms) with MOSI pulled up, matching
    /// the current FreeInk probe. If both look like a floating line, one more
    /// 50 ms pass leaves MOSI floating so a weak driver is not hidden by the pull.
    pub async fn probe(&mut self) -> Controller {
        let short = self.pass(1, Pull::Up).await;
        self.log_pass("1ms pull-up", &short);
        Timer::after(Duration::from_millis(2)).await;

        let long = self.pass(50, Pull::Up).await;
        self.log_pass("50ms pull-up", &long);

        let mut chosen = if !floating(&long) { long } else { short };
        if floating(&chosen) {
            Timer::after(Duration::from_millis(2)).await;
            let hiz = self.pass(50, Pull::None).await;
            self.log_pass("50ms hi-z", &hiz);
            if !floating(&hiz) {
                chosen = hiz;
            }
        }

        match chosen.ver[2] {
            0x01 if !floating(&chosen) => Controller::Uc8179,
            0x02 | 0x03 | 0x67 | 0x68 | 0x69 if !floating(&chosen) => Controller::Uc8279,
            _ => Controller::Unknown(chosen.ver),
        }
    }

    /// OTP full refresh of one portrait frame. The bytes are 480×800, row-major,
    /// MSB left, 1 = white. X4 Classic does not program PLL.
    pub async fn refresh_portrait(&mut self, frame: &[u8]) {
        let bytes = crate::board::VIEW_WIDTH * crate::board::VIEW_HEIGHT / 8;
        if frame.len() != bytes {
            info!("X4 frame is {} bytes, want {bytes}", frame.len());
            return;
        }
        info!("X4 UC8279 reset");
        self.reset_panel(50).await;
        let idle = self.wait_level(true, 300).await;
        info!("X4 UC8279 idle after reset busy_high={idle}");

        self.command_bytes(0x00, &[0x37, 0x4D]);
        self.command_bytes(0x61, &[0x03, 0x20, 0x02, 0x58]);
        self.command_bytes(0x65, &[0x00, 0x00, 0x00, 0x00]);
        self.command_bytes(0x03, &[0x20]);
        self.command_bytes(0xE1, &[0x02]);

        info!("X4 UC8279 write old plane white");
        self.write_solid(0x10, 0xFF, ADDRESSED_GATES).await;
        info!("X4 UC8279 write new plane");
        self.write_frame(0x13, &frame).await;

        self.command_bytes(0x50, &[0x97]);
        self.command_bytes(0xE0, &[0x02]);
        self.command_bytes(0xE5, &[0x1E]);
        info!("X4 UC8279 power on");
        self.command(0x04);
        let pon_low = self.wait_level(false, 40).await;
        let pon_start = Instant::now();
        let powered = self.wait_level(true, 1_500).await;
        info!(
            "X4 UC8279 power on saw_busy={} idle={} {}ms",
            pon_low,
            powered,
            pon_start.elapsed().as_millis()
        );
        if !powered {
            info!("X4 UC8279 power on timed out — refresh not started");
            return;
        }

        // PON reloads MTP. PSR written after that is the one that latches,
        // with REG cleared so the refresh uses the OTP waveform.
        self.command_bytes(0x00, &[0x17, 0x4D]);
        info!("X4 UC8279 refresh");
        self.command(0x12);
        let refresh_at = Instant::now();
        let started = self.wait_level(false, 200).await;
        let done = self.wait_level(true, 8_000).await;
        info!(
            "X4 UC8279 refresh started={started} done={done} {}ms",
            refresh_at.elapsed().as_millis()
        );
        if !done {
            info!("X4 UC8279 refresh timed out — leaving the booster on");
            return;
        }

        info!("X4 UC8279 sync old plane");
        self.write_frame(0x10, &frame).await;
        self.command(0x02);
        let off_low = self.wait_level(false, 40).await;
        let off = self.wait_level(true, 1_500).await;
        info!("X4 UC8279 power off saw_busy={off_low} idle={off}");
    }

    /// Controller deep sleep. The image stays on the glass. The next refresh
    /// resets the chip, which is the only way out of this command.
    pub fn sleep_controller(&mut self) {
        info!("X4 panel controller sleep");
        self.command_bytes(0x07, &[0xA5]);
    }

    pub async fn show_uc8279_mark(&mut self) {
        let mut frame =
            alloc::vec![0xFFu8; crate::board::VIEW_WIDTH * crate::board::VIEW_HEIGHT / 8];
        draw_mark(&mut frame);
        self.refresh_portrait(&frame).await;
    }

    fn log_pass(&self, label: &str, pass: &Pass) {
        let ver = pass.ver;
        info!(
            "X4 probe {label} VER={:02x} {:02x} {:02x} {:02x} {:02x} FLG={:02x} busy={}",
            ver[0],
            ver[1],
            ver[2],
            ver[3],
            ver[4],
            pass.flg,
            pass.busy_high
        );
    }

    async fn pass(&mut self, rst_low_ms: u64, pull: Pull) -> Pass {
        self.cs.set_high();
        self.sclk.set_low();
        self.dc.set_low();
        self.mosi_output();

        self.rst.set_high();
        Timer::after(Duration::from_millis(2)).await;
        self.rst.set_low();
        Timer::after(Duration::from_millis(rst_low_ms)).await;
        self.rst.set_high();
        Timer::after(Duration::from_millis(30)).await;

        let busy_high = self.busy.is_high();
        let mut flg = [0u8; 1];
        self.cmd_read(0x71, &mut flg, pull);
        let mut ver = [0u8; 5];
        self.cmd_read(0x70, &mut ver, pull);
        Pass {
            ver,
            flg: flg[0],
            busy_high,
        }
    }

    fn cmd_read(&mut self, cmd: u8, out: &mut [u8], pull: Pull) {
        self.mosi_output();
        self.sclk.set_low();
        self.dc.set_low();
        self.cs.set_low();
        tick();
        self.write_byte(cmd);
        self.dc.set_high();
        self.mosi_input(pull);
        tick();
        for slot in out.iter_mut() {
            *slot = self.read_byte();
        }
        self.cs.set_high();
        self.mosi_output();
    }

    fn write_byte(&mut self, mut byte: u8) {
        for _ in 0..8 {
            self.mosi.set_level(level_from(byte & 0x80 != 0));
            tick();
            self.sclk.set_high();
            tick();
            self.sclk.set_low();
            byte <<= 1;
        }
    }

    fn read_byte(&mut self) -> u8 {
        let mut byte = 0u8;
        for _ in 0..8 {
            tick();
            byte = (byte << 1) | u8::from(self.mosi.is_high());
            self.sclk.set_high();
            tick();
            self.sclk.set_low();
        }
        byte
    }

    fn mosi_output(&mut self) {
        self.mosi.set_input_enable(false);
        self.mosi
            .apply_input_config(&InputConfig::default().with_pull(Pull::None));
        self.mosi.set_low();
        self.mosi.set_output_enable(true);
    }

    fn mosi_input(&mut self, pull: Pull) {
        self.mosi.set_output_enable(false);
        self.mosi
            .apply_input_config(&InputConfig::default().with_pull(pull));
        self.mosi.set_input_enable(true);
    }

    async fn reset_panel(&mut self, low_ms: u64) {
        self.cs.set_high();
        self.sclk.set_low();
        self.dc.set_low();
        self.mosi_output();
        self.rst.set_high();
        Timer::after(Duration::from_millis(10)).await;
        self.rst.set_low();
        Timer::after(Duration::from_millis(low_ms)).await;
        self.rst.set_high();
        Timer::after(Duration::from_millis(50)).await;
    }

    async fn wait_level(&mut self, high: bool, timeout_ms: u64) -> bool {
        let start = Instant::now();
        let limit = Duration::from_millis(timeout_ms);
        loop {
            if self.busy.is_high() == high {
                return true;
            }
            if start.elapsed() >= limit {
                return false;
            }
            Timer::after(Duration::from_millis(1)).await;
        }
    }

    fn command(&mut self, cmd: u8) {
        self.command_bytes(cmd, &[]);
    }

    fn command_bytes(&mut self, cmd: u8, data: &[u8]) {
        self.mosi_output();
        self.sclk.set_low();
        self.dc.set_low();
        self.cs.set_low();
        tick();
        self.write_byte(cmd);
        self.cs.set_high();
        if data.is_empty() {
            return;
        }
        self.dc.set_high();
        self.cs.set_low();
        tick();
        for byte in data {
            self.write_byte(*byte);
        }
        self.cs.set_high();
    }

    async fn write_solid(&mut self, cmd: u8, byte: u8, rows: usize) {
        self.begin_data(cmd);
        for row in 0..rows {
            for _ in 0..NATIVE_STRIDE {
                self.write_byte(byte);
            }
            if row % 30 == 0 {
                Timer::after(Duration::from_millis(1)).await;
            }
        }
        self.cs.set_high();
    }

    async fn write_frame(&mut self, cmd: u8, frame: &[u8]) {
        self.begin_data(cmd);
        for _ in 0..GATE_OFFSET {
            for _ in 0..NATIVE_STRIDE {
                self.write_byte(0xFF);
            }
        }
        Timer::after(Duration::from_millis(1)).await;
        // Portrait column lx is constant across one native row.
        for ny in 0..crate::board::NATIVE_HEIGHT {
            let lx = crate::board::NATIVE_HEIGHT - 1 - ny;
            for byte_index in 0..NATIVE_STRIDE {
                let mut bits = 0xFFu8;
                for bit in 0..8 {
                    let ly = byte_index * 8 + bit;
                    if is_black(frame, lx, ly) {
                        bits &= !(0x80 >> bit);
                    }
                }
                self.write_byte(bits);
            }
            if ny % 24 == 0 {
                Timer::after(Duration::from_millis(1)).await;
            }
        }
        self.cs.set_high();
    }

    fn begin_data(&mut self, cmd: u8) {
        self.command(cmd);
        self.mosi_output();
        self.dc.set_high();
        self.cs.set_low();
        tick();
    }
}

const NATIVE_STRIDE: usize = crate::board::NATIVE_WIDTH / 8;
const VIEW_STRIDE: usize = crate::board::VIEW_WIDTH / 8;
const GATE_OFFSET: usize = 120;
const ADDRESSED_GATES: usize = 600;

fn draw_mark(frame: &mut [u8]) {
    for y in 0..crate::board::VIEW_HEIGHT {
        for x in 0..crate::board::VIEW_WIDTH {
            let border = x < 8 || y < 16 || x >= crate::board::VIEW_WIDTH - 8 || y >= crate::board::VIEW_HEIGHT - 8;
            if border {
                set_black(frame, x, y);
            }
        }
    }
    fill_black(frame, 28, 36, 56, 56);
    blit(frame, &GLYPH_X, 148, 300, 16);
    blit(frame, &GLYPH_4, 252, 300, 16);
    let small = [&GLYPH_U, &GLYPH_C, &GLYPH_8, &GLYPH_2, &GLYPH_7, &GLYPH_9];
    for (index, glyph) in small.iter().enumerate() {
        blit(frame, glyph, 114 + index * 42, 460, 6);
    }
}

fn fill_black(frame: &mut [u8], x0: usize, y0: usize, w: usize, h: usize) {
    for y in y0..y0 + h {
        for x in x0..x0 + w {
            set_black(frame, x, y);
        }
    }
}

fn set_black(frame: &mut [u8], x: usize, y: usize) {
    if x >= crate::board::VIEW_WIDTH || y >= crate::board::VIEW_HEIGHT {
        return;
    }
    let index = y * VIEW_STRIDE + x / 8;
    frame[index] &= !(0x80 >> (x % 8));
}

fn is_black(frame: &[u8], x: usize, y: usize) -> bool {
    if x >= crate::board::VIEW_WIDTH || y >= crate::board::VIEW_HEIGHT {
        return false;
    }
    let index = y * VIEW_STRIDE + x / 8;
    frame[index] & (0x80 >> (x % 8)) == 0
}

fn blit(frame: &mut [u8], glyph: &[u8; 7], x0: usize, y0: usize, scale: usize) {
    for row in 0..7 {
        for col in 0..5 {
            if glyph[row] & (1 << (4 - col)) == 0 {
                continue;
            }
            fill_black(
                frame,
                x0 + col * scale,
                y0 + row * scale,
                scale,
                scale,
            );
        }
    }
}

const GLYPH_X: [u8; 7] = [
    0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001,
];
const GLYPH_4: [u8; 7] = [
    0b00100, 0b01100, 0b10100, 0b11111, 0b00100, 0b00100, 0b00100,
];
const GLYPH_U: [u8; 7] = [
    0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
];
const GLYPH_C: [u8; 7] = [
    0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110,
];
const GLYPH_8: [u8; 7] = [
    0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
];
const GLYPH_2: [u8; 7] = [
    0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
];
const GLYPH_7: [u8; 7] = [
    0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
];
const GLYPH_9: [u8; 7] = [
    0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b10001, 0b01110,
];

fn floating(pass: &Pass) -> bool {
    let ver_flat = pass.ver.iter().all(|byte| *byte == pass.ver[0]);
    let flg_flat = pass.flg == 0x00 || pass.flg == 0xFF;
    ver_flat && flg_flat
}

fn level_from(high: bool) -> esp_hal::gpio::Level {
    if high {
        esp_hal::gpio::Level::High
    } else {
        esp_hal::gpio::Level::Low
    }
}

fn tick() {
    Delay::new().delay_micros(1);
}
