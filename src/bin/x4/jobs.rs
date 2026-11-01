//! The HTTP task only enqueues. The main task owns the panel.

use alloc::string::String;
use alloc::vec::Vec;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;

pub const TEXT_MAX: usize = 800;

pub enum Job {
    Frame(Vec<u8>),
    Slot(usize),
}

struct State {
    ip: String,
    last: String,
    refreshing: bool,
    pending: bool,
}

static STATE: Mutex<CriticalSectionRawMutex, State> = Mutex::new(State {
    ip: String::new(),
    last: String::new(),
    refreshing: false,
    pending: false,
});
static PENDING: Mutex<CriticalSectionRawMutex, Option<Job>> = Mutex::new(None);
static KICK: Signal<CriticalSectionRawMutex, ()> = Signal::new();

pub async fn set_ip(ip: String) {
    STATE.lock().await.ip = ip;
}

pub async fn set_refreshing(refreshing: bool) {
    STATE.lock().await.refreshing = refreshing;
}

pub struct Status {
    pub ip: String,
    pub last: String,
    pub refreshing: bool,
    pub pending: bool,
}

pub async fn status() -> Status {
    let state = STATE.lock().await;
    Status {
        ip: state.ip.clone(),
        last: state.last.clone(),
        refreshing: state.refreshing,
        pending: state.pending,
    }
}

/// Keep the newest frame that has not started. A refresh already on the glass
/// runs to the end.
pub async fn enqueue(job: Job, label: &str) {
    *PENDING.lock().await = Some(job);
    let mut state = STATE.lock().await;
    state.pending = true;
    state.last.clear();
    state.last.push_str(label);
    drop(state);
    KICK.signal(());
}

pub async fn wait() {
    KICK.wait().await;
}

pub async fn take() -> Option<Job> {
    let job = PENDING.lock().await.take();
    if job.is_some() {
        STATE.lock().await.pending = false;
    }
    job
}
