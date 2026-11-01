//! Portrait frames kept for `show_bitmap`. The side buttons do not walk them yet.

use alloc::vec::Vec;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;

use crate::board;

pub const WIDTH: usize = board::VIEW_WIDTH;
pub const HEIGHT: usize = board::VIEW_HEIGHT;
pub const STRIDE: usize = WIDTH / 8;
pub const BYTES: usize = STRIDE * HEIGHT;
pub const CAPACITY: usize = 8;

struct Gallery {
    buf: Vec<u8>,
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

pub async fn reserve() {
    let mut gallery = GALLERY.lock().await;
    if gallery.capacity == CAPACITY {
        return;
    }
    let mut buf = Vec::new();
    if buf.try_reserve_exact(CAPACITY * BYTES).is_err() {
        log::error!("X4 gallery reserve of {CAPACITY} frames failed");
        return;
    }
    buf.resize(CAPACITY * BYTES, 0xFF);
    gallery.buf = buf;
    gallery.capacity = CAPACITY;
    log::info!("X4 gallery {WIDTH}x{HEIGHT} ({BYTES} bytes) x{CAPACITY}");
}

pub async fn info() -> Info {
    let gallery = GALLERY.lock().await;
    Info {
        capacity: gallery.capacity,
        count: gallery.count,
        shown: gallery.shown,
    }
}

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

pub async fn copy(slot: usize) -> Option<Vec<u8>> {
    let gallery = GALLERY.lock().await;
    if slot >= gallery.count {
        return None;
    }
    let start = slot * BYTES;
    Some(gallery.buf[start..start + BYTES].to_vec())
}
