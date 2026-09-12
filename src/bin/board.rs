//! CrowPanel 5.79″ e-paper HMI (Elecrow DIS08792E).
//!
//! MCU: ESP32-S3-WROOM-1-N8R8 (8 MB flash, 8 MB octal PSRAM).
//! Panel: 272 × 792 B/W, dual SSD1683, 4-wire SPI.
//!
//! Display SPI (FSPI):
//! - MOSI 11, CLK 12, CS 45, DC 46, RST 47, BUSY 48, PWR 7
//!
//! Buttons / dial (active low, pull-up):
//! - EXIT 1, MENU 2, dial down 4, dial push 5, dial up 6
//!
//! Other:
//! - LED 41
//! - TF card SPI: MOSI 40, MISO 13, CLK 39, CS 10, power 42

#![allow(dead_code)]

pub const WIDTH: usize = 792;
pub const HEIGHT: usize = 272;

pub const GPIO_EXIT: u8 = 1;
pub const GPIO_MENU: u8 = 2;
pub const GPIO_DIAL_DOWN: u8 = 4;
pub const GPIO_DIAL_PUSH: u8 = 5;
pub const GPIO_DIAL_UP: u8 = 6;
pub const GPIO_EPD_PWR: u8 = 7;
pub const GPIO_EPD_MOSI: u8 = 11;
pub const GPIO_EPD_CLK: u8 = 12;
pub const GPIO_LED: u8 = 41;
pub const GPIO_EPD_CS: u8 = 45;
pub const GPIO_EPD_DC: u8 = 46;
pub const GPIO_EPD_RST: u8 = 47;
pub const GPIO_EPD_BUSY: u8 = 48;
