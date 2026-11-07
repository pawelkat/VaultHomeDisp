//! GATT note service. The phone writes markdown in chunks, then commits.
//! That uses the same page layout as `show_text`. This binary has no Wi-Fi.
//!
//! Service UUID `8a1f0001-4b6a-4c1e-9c3a-6d5e7f8091a2`
//! - `8a1f0002-...` text, write. Each write is appended.
//! - `8a1f0003-...` commit, write. `0` clears the buffer, `1` shows the note.
//! - `8a1f0004-...` status, read and notify. `pages:N` or a short error.

use alloc::string::String;
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync07::blocking_mutex::raw::NoopRawMutex;
use heapless::Vec as HeapVec;
use log::{info, warn};
use trouble_host::prelude::*;

use crate::gallery;
use crate::jobs::{self, Job, TEXT_MAX};
use crate::text;

const CONNECTIONS_MAX: usize = 1;
const L2CAP_CHANNELS_MAX: usize = 2;
const ATT_MAX: usize = 32;
const CCCD_MAX: usize = 4;
const CHUNK: usize = 244;

type AttServer = AttributeServer<'static, NoopRawMutex, DefaultPacketPool, ATT_MAX, CCCD_MAX, CONNECTIONS_MAX>;

struct NoteChars {
    text: Characteristic<HeapVec<u8, CHUNK>>,
    commit: Characteristic<u8>,
    status: Characteristic<HeapVec<u8, 24>>,
}

static INCOMING: Mutex<CriticalSectionRawMutex, alloc::vec::Vec<u8>> = Mutex::new(alloc::vec::Vec::new());

#[embassy_executor::task]
pub async fn run(connector: esp_radio::ble::controller::BleConnector<'static>) {
    let controller = ExternalController::<_, 8>::new(connector);
    static RESOURCES: static_cell::StaticCell<
        HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX>,
    > = static_cell::StaticCell::new();
    let resources = RESOURCES.init(HostResources::new());
    // Static random address. The two most significant bits of the address must be 1.
    let address = Address::random([0xfc, 0x4e, 0x2d, 0x6b, 0x45, 0xc2]);
    let stack = trouble_host::new(controller, resources).set_random_address(address);
    let host = stack.build();
    let mut runner = host.runner;
    let mut peripheral = host.peripheral;

    static TEXT_STORE: static_cell::StaticCell<[u8; CHUNK]> = static_cell::StaticCell::new();
    static COMMIT_STORE: static_cell::StaticCell<[u8; 1]> = static_cell::StaticCell::new();
    static STATUS_STORE: static_cell::StaticCell<[u8; 24]> = static_cell::StaticCell::new();
    let text_store = TEXT_STORE.init([0; CHUNK]);
    let commit_store = COMMIT_STORE.init([0]);
    let status_store = STATUS_STORE.init([0; 24]);

    let (server, notes) = build_server(text_store, commit_store, status_store);
    // Flags must be first: length 2, type 0x01, LE General Discoverable | BR/EDR Not Supported.
    // The name sits in the same payload so a passive scan can read it.
    const NAME: &[u8] = b"VaultHome X4";
    let mut adv_data = [0u8; 31];
    adv_data[0] = 0x02;
    adv_data[1] = 0x01;
    adv_data[2] = 0x06;
    adv_data[3] = (NAME.len() + 1) as u8;
    adv_data[4] = 0x09;
    adv_data[5..5 + NAME.len()].copy_from_slice(NAME);
    let adv_len = 5 + NAME.len();
    info!("X4 BLE advertising as VaultHome X4, payload {adv_len} bytes");

    let app = async {
        loop {
            let advertiser = match peripheral
                .advertise(
                    &Default::default(),
                    Advertisement::ConnectableScannableUndirected {
                        adv_data: &adv_data[..adv_len],
                        scan_data: &[],
                    },
                )
                .await
            {
                Ok(advertiser) => advertiser,
                Err(err) => {
                    warn!("X4 BLE advertise failed: {err:?}");
                    embassy_time::Timer::after(embassy_time::Duration::from_secs(1)).await;
                    continue;
                }
            };
            info!("X4 BLE advertise started");
            let Ok(acceptor) = advertiser.accept().await else {
                warn!("X4 BLE accept failed");
                continue;
            };
            let Ok(conn) = acceptor.with_attribute_server(&server) else {
                warn!("X4 BLE attribute server rejected the link");
                continue;
            };
            info!("X4 BLE connected");
            serve(&notes, &conn).await;
            info!("X4 BLE disconnected");
        }
    };
    match select(runner.run(), app).await {
        Either::First(Ok(())) => warn!("X4 BLE host ended"),
        Either::First(Err(err)) => warn!("X4 BLE host stopped: {err:?}"),
        Either::Second(()) => warn!("X4 BLE app ended"),
    }
    core::future::pending::<()>().await;
}

fn build_server(
    text_store: &'static mut [u8],
    commit_store: &'static mut [u8],
    status_store: &'static mut [u8],
) -> (AttServer, NoteChars) {
    let mut table: AttributeTable<'static, NoopRawMutex, ATT_MAX> = AttributeTable::new();
    let text;
    let commit;
    let status;
    {
        let mut service = table.add_service(Service {
            uuid: uuid("8a1f0001-4b6a-4c1e-9c3a-6d5e7f8091a2"),
        });
        text = service
            .add_characteristic(
                uuid("8a1f0002-4b6a-4c1e-9c3a-6d5e7f8091a2"),
                [CharacteristicProp::Write, CharacteristicProp::WriteWithoutResponse],
                HeapVec::<u8, CHUNK>::new(),
                text_store,
            )
            .build();
        commit = service
            .add_characteristic(
                uuid("8a1f0003-4b6a-4c1e-9c3a-6d5e7f8091a2"),
                [CharacteristicProp::Write, CharacteristicProp::WriteWithoutResponse],
                0u8,
                commit_store,
            )
            .build();
        status = service
            .add_characteristic(
                uuid("8a1f0004-4b6a-4c1e-9c3a-6d5e7f8091a2"),
                [CharacteristicProp::Read, CharacteristicProp::Notify],
                HeapVec::<u8, 24>::new(),
                status_store,
            )
            .build();
    }
    let server = AttributeServer::new(table);
    (server, NoteChars { text, commit, status })
}

fn uuid(text: &str) -> Uuid {
    let mut raw = [0u8; 16];
    let mut index = 0;
    let mut acc = 0u8;
    let mut high = true;
    for byte in text.bytes() {
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => continue,
        };
        if high {
            acc = digit << 4;
            high = false;
        } else {
            raw[index] = acc | digit;
            index += 1;
            high = true;
        }
    }
    // 128-bit UUIDs go on the air little-endian.
    raw.reverse();
    Uuid::new_long(raw)
}

async fn serve<P: PacketPool>(notes: &NoteChars, conn: &GattConnection<'_, '_, P>) {
    loop {
        match conn.next().await {
            GattConnectionEvent::Disconnected { reason } => {
                info!("X4 BLE disconnect {reason:?}");
                break;
            }
            GattConnectionEvent::Gatt { event } => {
                if let Err(err) = on_gatt(notes, conn, event).await {
                    warn!("X4 BLE gatt {err:?}");
                    break;
                }
            }
            _ => {}
        }
    }
}

async fn on_gatt<P: PacketPool>(
    notes: &NoteChars,
    conn: &GattConnection<'_, '_, P>,
    event: GattEvent<'_, '_, P>,
) -> Result<(), Error> {
    let GattEvent::Write(event) = event else {
        event.accept()?.send().await;
        return Ok(());
    };
    let handle = event.handle();
    let bytes = event.data().to_vec();
    event.accept()?.send().await;
    if handle == notes.text.handle {
        append_text(&bytes).await;
    } else if handle == notes.commit.handle {
        let command = bytes.first().copied().unwrap_or(0);
        let status = match command {
            0 => {
                INCOMING.lock().await.clear();
                status_msg(b"cleared")
            }
            1 => match publish().await {
                Ok(pages) => {
                    info!("X4 BLE note queued, {pages} page(s)");
                    pages_msg(pages)
                }
                Err(msg) => {
                    warn!("X4 BLE note rejected: {msg}");
                    status_msg(msg.as_bytes())
                }
            },
            _ => status_msg(b"bad commit"),
        };
        let _ = notes.status.notify(conn, &status).await;
    }
    Ok(())
}

async fn append_text(bytes: &[u8]) {
    let mut incoming = INCOMING.lock().await;
    if incoming.len() >= TEXT_MAX {
        return;
    }
    let room = TEXT_MAX - incoming.len();
    let take = bytes.len().min(room);
    incoming.extend_from_slice(&bytes[..take]);
}

async fn publish() -> Result<usize, &'static str> {
    let bytes = core::mem::take(&mut *INCOMING.lock().await);
    let text = core::str::from_utf8(&bytes).map_err(|_| "text is not utf-8")?;
    let text = text.trim();
    if text.is_empty() {
        return Err("empty note");
    }
    if text.chars().count() > TEXT_MAX {
        return Err("text is too long");
    }
    let pages = text::render_pages(text);
    let count = pages.len();
    if count == 0 {
        return Err("empty note");
    }
    gallery::begin_set().await;
    for (index, page) in pages.iter().enumerate() {
        gallery::store(Some(index), page).await?;
    }
    gallery::mark_shown(0).await;
    let label: String = text.chars().take(40).collect();
    jobs::enqueue(Job::Slot(0), &label).await;
    Ok(count)
}

fn pages_msg(pages: usize) -> HeapVec<u8, 24> {
    let text = alloc::format!("pages:{pages}");
    status_msg(text.as_bytes())
}

fn status_msg(bytes: &[u8]) -> HeapVec<u8, 24> {
    let mut out = HeapVec::new();
    let take = bytes.len().min(24);
    let _ = out.extend_from_slice(&bytes[..take]);
    out
}
