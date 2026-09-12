//! Dual SSD1683 driver for the CrowPanel 5.79″ (792 × 272 B/W).
//!
//! Protocol matches the working ESPHome CrowPanel 5.79 driver: two chips on
//! one SPI bus, shared CS/DC/RST/BUSY, slave commands 0x91/0xA4/0xA6/0xC4…,
//! master commands 0x11/0x24/0x26/0x44…. Byte 49 of each row is the 8-pixel
//! seam and is sent to both chips.

use embassy_time::{Duration, Instant, Timer};
use esp_hal::gpio::{Input, Output};
use esp_hal::spi::master::Spi;
use esp_hal::{Blocking, DriverMode};
use log::{error, info};

use crate::board::{HEIGHT, WIDTH};

pub const BYTES_PER_ROW: usize = WIDTH / 8;
pub const BUFFER_LEN: usize = BYTES_PER_ROW * HEIGHT;
const HALF_ROW: usize = 50;
const CHIP_RAM: usize = HALF_ROW * HEIGHT;

pub struct Epd<'d, Dm: DriverMode = Blocking> {
    spi: Spi<'d, Dm>,
    cs: Output<'d>,
    dc: Output<'d>,
    rst: Output<'d>,
    busy: Input<'d>,
}

impl<'d, Dm: DriverMode> Epd<'d, Dm> {
    pub fn new(
        spi: Spi<'d, Dm>,
        cs: Output<'d>,
        dc: Output<'d>,
        rst: Output<'d>,
        busy: Input<'d>,
    ) -> Self {
        Self {
            spi,
            cs,
            dc,
            rst,
            busy,
        }
    }

    pub async fn init(&mut self) {
        self.reset().await;

        self.cmd(0x12);
        self.wait_busy().await;

        self.cmd(0x18);
        self.data(0x80);

        self.cmd(0x22);
        self.data(0xB1);
        self.cmd(0x20);
        self.wait_busy().await;

        self.cmd(0x1A);
        self.data(0x64);
        self.data(0x00);

        self.cmd(0x22);
        self.data(0x91);
        self.cmd(0x20);
        self.wait_busy().await;

        self.cmd(0x3C);
        self.data(0x03);
        self.wait_busy().await;

        self.set_ram_master();
        self.write_ram_fill(0x24, 0x00, CHIP_RAM);
        self.set_ram_master();
        self.write_ram_fill(0x26, 0x00, CHIP_RAM);
        self.set_ram_slave();
        self.write_ram_fill(0xA4, 0x00, CHIP_RAM);
        self.set_ram_slave();
        self.write_ram_fill(0xA6, 0x00, CHIP_RAM);

        self.refresh_full().await;
        info!("SSD1683 init complete");
    }

    /// `buffer` is 99 bytes × 272 rows, MSB-first, bit 1 = white paper.
    pub async fn display(&mut self, buffer: &[u8]) {
        debug_assert_eq!(buffer.len(), BUFFER_LEN);

        self.set_ram_slave();
        self.cmd(0xA4);
        self.dc.set_high();
        self.cs.set_low();
        for y in 0..HEIGHT {
            let row = y * BYTES_PER_ROW;
            self.write_bytes(&buffer[row..row + HALF_ROW]);
        }
        self.cs.set_high();

        self.set_ram_master();
        self.cmd(0x24);
        self.dc.set_high();
        self.cs.set_low();
        for y in 0..HEIGHT {
            let row = y * BYTES_PER_ROW;
            self.write_bytes(&buffer[row + HALF_ROW - 1..row + BYTES_PER_ROW]);
        }
        self.cs.set_high();

        self.refresh_full().await;

        self.set_ram_slave();
        self.write_ram_fill(0xA6, 0x00, CHIP_RAM);
        self.set_ram_master();
        self.write_ram_fill(0x26, 0x00, CHIP_RAM);
        info!("SSD1683 full refresh done");
    }

    pub async fn deep_sleep(&mut self) {
        self.cmd(0x10);
        self.data(0x01);
        Timer::after(Duration::from_millis(100)).await;
    }

    async fn reset(&mut self) {
        Timer::after(Duration::from_millis(10)).await;
        self.rst.set_low();
        Timer::after(Duration::from_millis(10)).await;
        self.rst.set_high();
        Timer::after(Duration::from_millis(10)).await;
        self.wait_busy().await;
    }

    async fn refresh_full(&mut self) {
        self.cmd(0x22);
        self.data(0xF7);
        self.cmd(0x20);
        self.wait_busy().await;
    }

    async fn wait_busy(&mut self) {
        let start = Instant::now();
        while self.busy.is_high() {
            if start.elapsed() > Duration::from_secs(10) {
                error!("SSD1683 busy timeout");
                break;
            }
            Timer::after(Duration::from_millis(10)).await;
        }
    }

    fn cmd(&mut self, c: u8) {
        self.dc.set_low();
        self.cs.set_low();
        self.write_bytes(&[c]);
        self.cs.set_high();
    }

    fn data(&mut self, d: u8) {
        self.dc.set_high();
        self.cs.set_low();
        self.write_bytes(&[d]);
        self.cs.set_high();
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        self.spi.write(bytes).unwrap();
    }

    fn write_ram_fill(&mut self, cmd: u8, fill: u8, count: usize) {
        self.cmd(cmd);
        self.dc.set_high();
        self.cs.set_low();
        let chunk = [fill; 64];
        let mut left = count;
        while left > 0 {
            let n = left.min(chunk.len());
            self.write_bytes(&chunk[..n]);
            left -= n;
        }
        self.cs.set_high();
    }

    fn set_ram_master(&mut self) {
        self.cmd(0x11);
        self.data(0x02);
        self.cmd(0x44);
        self.data(0x31);
        self.data(0x00);
        self.cmd(0x45);
        self.data(0x00);
        self.data(0x00);
        self.data(0x0F);
        self.data(0x01);
        self.cmd(0x4E);
        self.data(0x31);
        self.cmd(0x4F);
        self.data(0x00);
        self.data(0x00);
    }

    fn set_ram_slave(&mut self) {
        self.cmd(0x91);
        self.data(0x03);
        self.cmd(0xC4);
        self.data(0x00);
        self.data(0x31);
        self.cmd(0xC5);
        self.data(0x00);
        self.data(0x00);
        self.data(0x0F);
        self.data(0x01);
        self.cmd(0xCE);
        self.data(0x00);
        self.cmd(0xCF);
        self.data(0x00);
        self.data(0x00);
    }
}
