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

The panel joins Wi-Fi, shows `http://<ip>/mcp` on a 24px line at the bottom, and serves streamable-HTTP MCP on port 80. `show_text` and `show_bitmap` queue a full refresh and return immediately. `show_bitmap` takes a 792×248 monochrome bitmap (the status line is not part of it) and can store 42 of them. `panel_status` reads IP, refresh state, and how many bitmaps are stored. The dial walks those slots: toward the bottom of the screen is next, toward the USB-C socket is previous. With two or more pages stored, the panel also advances by itself every 45 seconds and wraps to the first page. A dial press restarts that wait. The status line shows `n/total`. The onboard LED (GPIO41) blinks; MENU / EXIT / dial still log over UART. Liberation Sans is vendored under `ui/fonts/` (SIL OFL).
