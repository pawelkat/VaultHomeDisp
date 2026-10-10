//! Latest frame the panel should show.
//!
//! The HTTP task only enqueues. The main task owns the e-paper and applies
//! one full refresh at a time. A newer job replaces one that has not started.
//! Bitmap frames can become another [`PanelJob`] variant later; the queue
//! stays the same.

use alloc::string::String;
use core::sync::atomic::{AtomicU32, Ordering};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;

pub const TEXT_MAX: usize = 200;

pub enum PanelJob {
    Text(String),
    /// Slot in the bitmap gallery. The bytes stay there for a later
    /// next/previous walk; this step only shows the slot.
    Bitmap(usize),
}

struct PanelState {
    ip: String,
    last_text: String,
    refreshing: bool,
    pending: bool,
}

static STATE: Mutex<CriticalSectionRawMutex, PanelState> = Mutex::new(PanelState {
    ip: String::new(),
    last_text: String::new(),
    refreshing: false,
    pending: false,
});
static PENDING: Mutex<CriticalSectionRawMutex, Option<PanelJob>> = Mutex::new(None);
static KICK: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static EPOCH: AtomicU32 = AtomicU32::new(0);

pub fn epoch() -> u32 {
    EPOCH.load(Ordering::Relaxed)
}

pub async fn set_ip(ip: String) {
    STATE.lock().await.ip = ip;
}

pub async fn ip() -> String {
    STATE.lock().await.ip.clone()
}

pub async fn set_refreshing(refreshing: bool) {
    STATE.lock().await.refreshing = refreshing;
}

pub async fn note_shown(text: &str) {
    let mut state = STATE.lock().await;
    state.last_text.clear();
    state.last_text.push_str(text);
    state.refreshing = false;
}

pub struct Status {
    pub ip: String,
    pub last_text: String,
    pub refreshing: bool,
    pub pending: bool,
}

pub async fn status() -> Status {
    let state = STATE.lock().await;
    Status {
        ip: state.ip.clone(),
        last_text: state.last_text.clone(),
        refreshing: state.refreshing,
        pending: state.pending,
    }
}

/// Replace any not-yet-started job and wake the refresh loop.
pub async fn enqueue(job: PanelJob) {
    {
        let mut slot = PENDING.lock().await;
        *slot = Some(job);
    }
    STATE.lock().await.pending = true;
    EPOCH.fetch_add(1, Ordering::Relaxed);
    KICK.signal(());
}

pub async fn wait_kick() {
    KICK.wait().await;
}

pub async fn take() -> Option<PanelJob> {
    let job = PENDING.lock().await.take();
    if job.is_some() {
        STATE.lock().await.pending = false;
    }
    job
}
