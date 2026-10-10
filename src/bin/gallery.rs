//! Reserved 1-bit frames for the area above the status line.
//!
//! Slots stay in order. The dial walks `shown` from 0 to `count - 1`.
//! The bytes are reserved at boot so the advertised capacity is memory
//! that is actually held.

use alloc::vec::Vec;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;

use crate::board;

/// Bottom strip reserved for the connectivity line. Keep this equal to the
/// status rectangle in `ui/main.slint`.
pub const STATUS_H: usize = 24;
pub const WIDTH: usize = board::WIDTH;
pub const HEIGHT: usize = board::HEIGHT - STATUS_H;
pub const STRIDE: usize = WIDTH / 8;
pub const BYTES: usize = STRIDE * HEIGHT;
/// One mebibyte, only whole bitmaps.
pub const BUDGET: usize = 1024 * 1024;
pub const CAPACITY: usize = BUDGET / BYTES;
/// Gap between automatic page advances. The dial still steps immediately.
pub const AUTO_PAGE_SECS: u64 = 45;

const _: () = assert!(HEIGHT + STATUS_H == board::HEIGHT);
const _: () = assert!(WIDTH % 8 == 0);
const _: () = assert!(CAPACITY >= 1);
const _: () = assert!(CAPACITY * BYTES <= BUDGET);

struct Gallery {
    buf: Vec<u8>,
    /// Slots whose memory was actually reserved. Zero if the reserve failed.
    capacity: usize,
    count: usize,
    shown: Option<usize>,
}

static GALLERY: Mutex<CriticalSectionRawMutex, Gallery> = Mutex::new(Gallery {
    buf: Vec::new(),
    capacity: 0,
    count: 0,
    shown: None,
});

pub struct Info {
    pub capacity: usize,
    pub count: usize,
    pub shown: Option<usize>,
}

/// Hold the gallery bytes in PSRAM. Call once the PSRAM allocator is up.
pub async fn reserve() {
    let mut gallery = GALLERY.lock().await;
    if gallery.capacity == CAPACITY {
        return;
    }
    let mut buf = Vec::new();
    if buf.try_reserve_exact(CAPACITY * BYTES).is_err() {
        log::error!("bitmap gallery: reserve of {CAPACITY} slots failed");
        gallery.capacity = 0;
        return;
    }
    buf.resize(CAPACITY * BYTES, 0xFF);
    gallery.buf = buf;
    gallery.capacity = CAPACITY;
    log::info!(
        "bitmap gallery: {WIDTH}x{HEIGHT} stride {STRIDE} ({BYTES} bytes), {CAPACITY} slots"
    );
}

pub async fn info() -> Info {
    let gallery = GALLERY.lock().await;
    Info {
        capacity: gallery.capacity,
        count: gallery.count,
        shown: gallery.shown,
    }
}

/// Store `bytes` (must be [`BYTES`] long). `index` overwrites that slot.
/// Omit it to append. Returns the slot number.
/// Drop the visible set. Stored bytes stay allocated; the next stores start at slot 0.
pub async fn begin_set() {
    let mut gallery = GALLERY.lock().await;
    gallery.count = 0;
    gallery.shown = None;
}

pub async fn store(index: Option<usize>, bytes: &[u8]) -> Result<usize, &'static str> {
    if bytes.len() != BYTES {
        return Err("bitmap length");
    }
    let mut gallery = GALLERY.lock().await;
    if gallery.capacity == 0 {
        return Err("bitmap store is not available");
    }
    let slot = match index {
        None => {
            if gallery.count >= gallery.capacity {
                return Err("bitmap store is full");
            }
            let slot = gallery.count;
            gallery.count += 1;
            slot
        }
        Some(slot) if slot < gallery.capacity && slot <= gallery.count => {
            if slot == gallery.count {
                gallery.count += 1;
            }
            slot
        }
        Some(_) => return Err("bitmap index is out of range"),
    };
    let start = slot * BYTES;
    gallery.buf[start..start + BYTES].copy_from_slice(bytes);
    gallery.shown = Some(slot);
    Ok(slot)
}

/// Move the shown slot by `delta` and stop at the ends. `None` means there
/// is nothing stored, or the dial is already at that end.
pub async fn step(delta: isize) -> Option<usize> {
    if delta == 0 {
        return None;
    }
    let mut gallery = GALLERY.lock().await;
    if gallery.count == 0 {
        return None;
    }
    let current = gallery.shown.unwrap_or(0).min(gallery.count - 1);
    let next = if delta > 0 {
        current
            .saturating_add(delta as usize)
            .min(gallery.count - 1)
    } else {
        current.saturating_sub(delta.unsigned_abs())
    };
    if gallery.shown == Some(next) {
        return None;
    }
    gallery.shown = Some(next);
    Some(next)
}

/// Next stored page, wrapping to slot 0. `None` when fewer than two pages
/// are stored, so a single page is not refreshed on a timer.
pub async fn advance() -> Option<usize> {
    let mut gallery = GALLERY.lock().await;
    if gallery.count < 2 {
        return None;
    }
    let current = gallery.shown.unwrap_or(0) % gallery.count;
    let next = (current + 1) % gallery.count;
    gallery.shown = Some(next);
    Some(next)
}

pub async fn copy(index: usize) -> Option<Vec<u8>> {
    let gallery = GALLERY.lock().await;
    if index >= gallery.count {
        return None;
    }
    let start = index * BYTES;
    Some(gallery.buf[start..start + BYTES].to_vec())
}
