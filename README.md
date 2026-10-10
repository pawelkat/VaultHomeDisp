# VaultHomeDisp

Embassy + esp-hal firmware for the Elecrow **CrowPanel 5.79″ e-paper** (DIS08792E).

- MCU: ESP32-S3-WROOM-1-**N8R8** (8 MB flash, 8 MB octal PSRAM)
- Panel: 272 × 792 B/W, dual SSD1683
- Host: same machine as other ESP32 work (`Latitude 5300`)

## Build / flash

PSRAM init is required to run in **release**:

```bash
source ~/export-esp.sh
cd ~/IdeaProjects/crowpanel-embassy
cargo run --release
```

USB-C on the panel. Hold **BOOT**, tap **RST** if the port does not appear.

Wi-Fi credentials are read at build time from the environment (`WIFI_SSID`, `WIFI_PASSWORD`, optional `WIFI_SSID2` / `WIFI_PASSWORD2`) or from a gitignored `wifi.env` in this directory.

Visual orientation is **USB-C up**, buttons on the **left** (180° from the panel native scan). Slint (0,0) is that visual top-left.

The panel joins Wi-Fi, shows `http://<ip>/mcp`, and serves streamable-HTTP MCP on port 80. `show_text` queues a full refresh and returns immediately. `panel_status` reads IP and refresh state. A bitmap tool is not implemented yet. The onboard LED (GPIO41) blinks; MENU / EXIT / dial still log over UART. Liberation Sans is vendored under `ui/fonts/` (SIL OFL).
