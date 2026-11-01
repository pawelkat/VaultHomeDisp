//! Controller probe only. No waveform is sent until the ID is known on hardware.

use embassy_time::{Duration, Timer};
use esp_hal::gpio::{Input, Output};
use esp_hal::spi::master::Spi;
use esp_hal::{Blocking, DriverMode};
use log::info;

pub struct Panel<'d, Dm: DriverMode = Blocking> {
    spi: Spi<'d, Dm>,
    cs: Output<'d>,
    dc: Output<'d>,
    rst: Output<'d>,
    busy: Input<'d>,
}

pub enum Controller {
    Uc8179,
    Uc8279,
    Unknown([u8; 3]),
}

impl<'d, Dm: DriverMode> Panel<'d, Dm> {
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

    pub async fn probe(&mut self) -> Controller {
        self.reset().await;
        let id = self.read_id();
        info!(
            "X4 panel id {:02x} {:02x} {:02x} busy={}",
            id[0],
            id[1],
            id[2],
            self.busy.is_high()
        );
        match id[1] {
            0x01 | 0x40 => Controller::Uc8179,
            0x02 | 0x68 | 0x69 | 0x41 | 0x42 => Controller::Uc8279,
            _ => Controller::Unknown(id),
        }
    }

    async fn reset(&mut self) {
        self.rst.set_high();
        Timer::after(Duration::from_millis(10)).await;
        self.rst.set_low();
        Timer::after(Duration::from_millis(20)).await;
        self.rst.set_high();
        Timer::after(Duration::from_millis(20)).await;
    }

    fn read_id(&mut self) -> [u8; 3] {
        self.dc.set_low();
        self.cs.set_low();
        self.spi.write(&[0x70]).ok();
        self.cs.set_high();
        self.dc.set_high();
        let mut id = [0u8; 3];
        self.cs.set_low();
        // Half-duplex: the controller answers on MOSI. A MOSI-only SPI
        // often returns 0xFF here; the log is the first hardware check.
        self.spi.transfer(&mut id).ok();
        self.cs.set_high();
        id
    }
}
