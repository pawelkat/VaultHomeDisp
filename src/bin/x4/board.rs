//! XTEINK X4 Classic (ESP32-S3, 16 MB flash, 8 MB octal PSRAM).
//!
//! Pins from FreeInk `XTEINK_X4_CLASSIC` (MIT).
//! The controller scans 800×480 landscape. Reading orientation is portrait
//! 480×800: a native image looks rotated 90° clockwise when the long edge
//! is vertical, so portrait (x, y) is stored at native (y, 479 − x).

pub const NATIVE_WIDTH: usize = 800;
pub const NATIVE_HEIGHT: usize = 480;
pub const VIEW_WIDTH: usize = 480;
pub const VIEW_HEIGHT: usize = 800;

pub const GPIO_PWR: u8 = 1;
pub const GPIO_PREV: u8 = 0;
pub const GPIO_NEXT: u8 = 7;
pub const GPIO_LEFT: u8 = 5;
pub const GPIO_RIGHT: u8 = 2;
pub const GPIO_CONFIRM: u8 = 8;
pub const GPIO_BACK: u8 = 9;
pub const GPIO_POWER_KEY: u8 = 3;

pub const GPIO_EPD_CLK: u8 = 12;
pub const GPIO_EPD_MOSI: u8 = 11;
pub const GPIO_EPD_CS: u8 = 13;
pub const GPIO_EPD_DC: u8 = 14;
pub const GPIO_EPD_RST: u8 = 10;
pub const GPIO_EPD_BUSY: u8 = 18;

pub const GPIO_SD_PWR: u8 = 6;
