//! Station Wi-Fi. Credentials come from `wifi.env` at build time.

use alloc::string::{String, ToString};
use embassy_futures::select::{Either, select};
use embassy_net::{Runner, Stack};
use embassy_time::{Duration, Timer};
use log::{error, info, warn};

const WIFI_SSID: &str = env!("WIFI_SSID");
const WIFI_PASSWORD: &str = env!("WIFI_PASSWORD");
const WIFI_SSID2: &str = env!("WIFI_SSID2");
const WIFI_PASSWORD2: &str = env!("WIFI_PASSWORD2");

pub fn ssid_ready() -> bool {
    !WIFI_SSID.is_empty()
}

pub fn primary_ssid() -> &'static str {
    WIFI_SSID
}

pub fn fallback_ssid() -> &'static str {
    WIFI_SSID2
}

pub fn ipv4(stack: Stack<'_>) -> Option<String> {
    let cfg = stack.config_v4()?;
    let cidr = cfg.address.to_string();
    Some(cidr.split('/').next().unwrap_or(cidr.as_str()).to_string())
}

#[embassy_executor::task]
pub async fn net_task(mut runner: Runner<'static, esp_radio::wifi::Interface<'static>>) -> ! {
    runner.run().await
}

fn networks() -> [(&'static str, &'static str); 2] {
    [(WIFI_SSID, WIFI_PASSWORD), (WIFI_SSID2, WIFI_PASSWORD2)]
}

#[embassy_executor::task]
pub async fn wifi_task(mut controller: esp_radio::wifi::WifiController<'static>) {
    let nets = networks();
    let mut idx = 0usize;
    loop {
        let Some(j) = (0..nets.len())
            .map(|k| (idx + k) % nets.len())
            .find(|&n| !nets[n].0.is_empty())
        else {
            error!("No Wi-Fi SSID configured");
            return;
        };
        let (ssid, password) = nets[j];
        let station_config = esp_radio::wifi::sta::StationConfig::default()
            .with_ssid(ssid)
            .with_password(password.to_string());
        let radio_config = esp_radio::wifi::Config::Station(station_config);
        if let Err(e) = controller.set_config(&radio_config) {
            error!("Wi-Fi set_config `{ssid}` failed: {e:?}");
            idx = j.wrapping_add(1);
            Timer::after(Duration::from_secs(2)).await;
            continue;
        }
        let kind = if j == 0 { "primary" } else { "fallback" };
        info!("Connecting to `{ssid}` ({kind})");
        match select(
            controller.connect_async(),
            Timer::after(Duration::from_secs(15)),
        )
        .await
        {
            Either::First(Ok(_)) => {
                info!("Wi-Fi associated with `{ssid}`");
                let _ = controller.wait_for_disconnect_async().await;
                warn!("Wi-Fi disconnected from `{ssid}`");
                idx = 0;
                Timer::after(Duration::from_secs(2)).await;
            }
            Either::First(Err(e)) => {
                error!("Wi-Fi connect `{ssid}` failed: {e:?}");
                idx = j.wrapping_add(1);
                Timer::after(Duration::from_secs(5)).await;
            }
            Either::Second(()) => {
                warn!("Wi-Fi connect `{ssid}` timed out");
                let _ = controller.disconnect_async().await;
                idx = j.wrapping_add(1);
                Timer::after(Duration::from_secs(1)).await;
            }
        }
    }
}
