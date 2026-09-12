# crowpanel-embassy

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

v1 drives the dual SSD1683 (792 × 272 B/W) and paints **hi Pawel** with Slint's software renderer, then deep-sleeps the panel. The onboard LED (GPIO41) still blinks; MENU / EXIT / dial still log over UART. Liberation Sans is vendored under `ui/fonts/` (SIL OFL).
