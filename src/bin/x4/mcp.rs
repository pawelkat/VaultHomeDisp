//! Streamable-HTTP MCP on TCP :80 (`POST /mcp`).
//!
//! The refresh stays on the main task. This task only stores the frame and returns.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use embassy_futures::join::join;
use embassy_net::Stack;
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, with_timeout};
use embedded_io_async::Write;
use log::{info, warn};
use serde_json::{Value, json};
use static_cell::StaticCell;

use base64::Engine;

use crate::gallery;
use crate::jobs::{self, Job, TEXT_MAX};
use crate::text;

/// One portrait bitmap is 64 KiB of base64, plus the JSON wrapper.
const REQ_LEN: usize = 128 * 1024;

const MCP_PORT: u16 = 80;
const MAX_REQUESTS: usize = 32;
const PROTOCOLS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];
const PROTOCOL_DEFAULT: &str = "2025-03-26";

static SESSION: AtomicU32 = AtomicU32::new(0);
static SESSION_NONCE: AtomicU32 = AtomicU32::new(0xA5C3_0001);

pub fn seed_session(nonce: u32) {
    if nonce != 0 {
        SESSION_NONCE.store(nonce, Ordering::Relaxed);
    }
}

#[embassy_executor::task]
pub async fn run(stack: Stack<'static>) -> ! {
    static RX_A: StaticCell<[u8; 4096]> = StaticCell::new();
    static TX_A: StaticCell<[u8; 4096]> = StaticCell::new();
    static RX_B: StaticCell<[u8; 4096]> = StaticCell::new();
    static TX_B: StaticCell<[u8; 4096]> = StaticCell::new();
    let rx_a = RX_A.init([0; 4096]);
    let tx_a = TX_A.init([0; 4096]);
    let rx_b = RX_B.init([0; 4096]);
    let tx_b = TX_B.init([0; 4096]);
    let mut req = alloc::vec![0u8; REQ_LEN];

    info!("MCP listening on TCP :{MCP_PORT} (POST /mcp)");
    loop {
        let mut current = TcpSocket::new(stack, rx_a, tx_a);
        if !accept_one(&mut current).await {
            continue;
        }
        serve(&mut current, &mut req).await;

        loop {
            let mut incoming = TcpSocket::new(stack, rx_b, tx_b);
            arm(&mut incoming);
            let ((), accepted) = join(drain(&mut current), incoming.accept(MCP_PORT)).await;
            drop(current);
            if let Err(e) = accepted {
                warn!("MCP accept error: {e:?}");
                drain(&mut incoming).await;
                break;
            }
            serve(&mut incoming, &mut req).await;

            let mut current_next = TcpSocket::new(stack, rx_a, tx_a);
            arm(&mut current_next);
            let ((), accepted) = join(drain(&mut incoming), current_next.accept(MCP_PORT)).await;
            drop(incoming);
            if let Err(e) = accepted {
                warn!("MCP accept error: {e:?}");
                drain(&mut current_next).await;
                break;
            }
            serve(&mut current_next, &mut req).await;
            current = current_next;
        }
    }
}

async fn accept_one(socket: &mut TcpSocket<'_>) -> bool {
    arm(socket);
    match socket.accept(MCP_PORT).await {
        Ok(()) => true,
        Err(e) => {
            warn!("MCP accept error: {e:?}");
            drain(socket).await;
            false
        }
    }
}

async fn serve(socket: &mut TcpSocket<'_>, req: &mut [u8]) {
    info!("MCP client {:?}", socket.remote_endpoint());
    if let Err(e) = handle_connection(socket, req).await {
        warn!("MCP connection error: {e}");
    }
    socket.close();
}

fn arm(socket: &mut TcpSocket<'_>) {
    socket.set_timeout(Some(Duration::from_secs(120)));
    socket.set_keep_alive(Some(Duration::from_secs(30)));
}

async fn drain(socket: &mut TcpSocket<'_>) {
    if with_timeout(Duration::from_millis(200), socket.flush())
        .await
        .is_err()
    {
        socket.abort();
        let _ = with_timeout(Duration::from_millis(100), socket.flush()).await;
    }
}

async fn handle_connection(socket: &mut TcpSocket<'_>, buf: &mut [u8]) -> Result<(), &'static str> {
    for _ in 0..MAX_REQUESTS {
        let n = match read_http(socket, buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err("read error") => break,
            Err("length required") => {
                write_http(
                    socket,
                    411,
                    Some("text/plain"),
                    b"Content-Length required\n",
                    None,
                )
                .await?;
                break;
            }
            Err("body too large") | Err("headers too large") => {
                write_http(socket, 413, Some("text/plain"), b"payload too large\n", None).await?;
                break;
            }
            Err(e) => return Err(e),
        };
        let req = core::str::from_utf8(&buf[..n]).map_err(|_| "request not utf-8")?;
        let Some((method, path, headers, body)) = split_http(req) else {
            write_http(socket, 400, Some("text/plain"), b"bad HTTP\n", None).await?;
            break;
        };
        let close = wants_close(headers);

        if method == "OPTIONS" && (path == "/" || path == "/mcp") {
            write_http(socket, 204, None, b"", None).await?;
        } else if method == "GET" && (path == "/" || path == "/mcp") {
            if accept_sse(headers) {
                write_http(socket, 405, None, b"", None).await?;
            } else {
                let help = help_text().await;
                write_http(socket, 200, Some("text/plain"), help.as_bytes(), None).await?;
            }
        } else if method == "DELETE" && (path == "/" || path == "/mcp") {
            if session_matches(headers) {
                SESSION.store(0, Ordering::Relaxed);
                info!("MCP session closed");
                write_http(socket, 200, None, b"", None).await?;
            } else {
                write_http(socket, 404, Some("text/plain"), b"unknown session\n", None).await?;
            }
        } else if method != "POST" || (path != "/mcp" && path != "/") {
            write_http(socket, 404, Some("text/plain"), b"not found\n", None).await?;
        } else if header_value(headers, "transfer-encoding").is_some() {
            write_http(
                socket,
                400,
                Some("text/plain"),
                b"chunked transfer is not supported\n",
                None,
            )
            .await?;
            break;
        } else if let Err(status) = check_protocol_header(headers) {
            write_http(
                socket,
                status,
                Some("text/plain"),
                b"unsupported protocol version\n",
                None,
            )
            .await?;
        } else {
            match handle_rpc(body, headers).await {
                RpcOut::Empty => write_http(socket, 202, None, b"", session_header()).await?,
                RpcOut::Json {
                    body,
                    session,
                    protocol,
                } => {
                    write_http_proto(
                        socket,
                        200,
                        Some("application/json"),
                        body.as_bytes(),
                        session,
                        protocol,
                    )
                    .await?;
                }
                RpcOut::NoSession => {
                    write_http(socket, 404, Some("text/plain"), b"unknown session\n", None).await?;
                }
            }
        }
        if close {
            break;
        }
    }
    Ok(())
}

enum RpcOut {
    Empty,
    Json {
        body: String,
        session: Option<u32>,
        protocol: &'static str,
    },
    NoSession,
}

async fn handle_rpc(body: &str, headers: &str) -> RpcOut {
    let body = body.trim();
    if body.starts_with('[') {
        return rpc_json(
            Value::Null,
            -32600,
            "JSON-RPC batches are not supported",
            None,
        );
    }
    let v: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            let preview: String = body.chars().take(80).collect();
            warn!("MCP JSON parse error: {e}; body={preview:?}");
            return rpc_json(Value::Null, -32700, "Parse error", None);
        }
    };
    if !v.is_object() {
        return rpc_json(Value::Null, -32600, "Invalid request", None);
    }
    if v.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        let id = v.get("id").cloned().unwrap_or(Value::Null);
        return rpc_json(id, -32600, "Invalid request", None);
    }

    let method = v.get("method").and_then(Value::as_str).unwrap_or("");
    let has_id = v.get("id").is_some();
    let id = v.get("id").cloned().unwrap_or(Value::Null);
    info!("MCP rpc {method}");

    if method.is_empty() {
        return rpc_json(id, -32600, "Invalid request", None);
    }
    if !has_id {
        let session_open = SESSION.load(Ordering::Relaxed) != 0;
        if method != "initialize" && session_open && !session_matches(headers) {
            return RpcOut::NoSession;
        }
        return RpcOut::Empty;
    }

    if method == "initialize" {
        let requested = v
            .pointer("/params/protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(PROTOCOL_DEFAULT);
        let Some(protocol) = PROTOCOLS.iter().copied().find(|p| *p == requested) else {
            return rpc_json(id, -32602, "Unsupported protocol version", None);
        };
        let session = issue_session();
        info!("MCP session {session:08x} protocol {protocol}");
        let result = json!({
            "protocolVersion": protocol,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "x4-epaper", "version": "0.1.0" },
            "instructions": instructions(),
        });
        return RpcOut::Json {
            body: rpc_ok(id, result),
            session: Some(session),
            protocol,
        };
    }

    if !session_matches(headers) {
        return RpcOut::NoSession;
    }
    let session = Some(SESSION.load(Ordering::Relaxed));
    let result = match method {
        "ping" => json!({}),
        "tools/list" => tools_list(),
        "tools/call" => match call_tool(&v).await {
            Ok(r) => r,
            Err(msg) => json!({
                "content": [{"type": "text", "text": msg}],
                "isError": true
            }),
        },
        "prompts/list" => json!({ "prompts": [] }),
        "resources/list" => json!({ "resources": [] }),
        "resources/templates/list" => json!({ "resourceTemplates": [] }),
        "logging/setLevel" => json!({}),
        _ => return rpc_json(id, -32601, "Method not found", session),
    };
    RpcOut::Json {
        body: rpc_ok(id, result),
        session,
        protocol: PROTOCOL_DEFAULT,
    }
}

fn rpc_json(id: Value, code: i32, message: &'static str, session: Option<u32>) -> RpcOut {
    RpcOut::Json {
        body: rpc_err(id, code, message),
        session,
        protocol: PROTOCOL_DEFAULT,
    }
}

fn instructions() -> String {
    alloc::format!(
        "This server updates the XTEINK X4 Classic e-paper. The page is portrait {}x{} pixels. \
         show_text lays out markdown on the device (headings, bullets, paragraphs, wikilink names) \
         and the side keys scroll the pages: up is previous, down is next, and a press at either end stays put. \
         show_text and show_bitmap queue a full refresh and return before the glass has finished. \
         A page takes several seconds. show_bitmap takes one monochrome bitmap ({} bytes, base64). \
         The panel stores {} pages. 1 = white, 0 = black, MSB is the left pixel. \
         Do not invent text or pixel data.",
        gallery::WIDTH,
        gallery::HEIGHT,
        gallery::BYTES,
        gallery::CAPACITY
    )
}

fn tools_list() -> Value {
    let text_desc = alloc::format!(
        "Lay out markdown on the X4 in portrait ({}x{}) and show the first page. \
         Headings, bullets, paragraphs, and wikilink visible names are kept. Links are not selectable. \
         The side key up moves to the previous page and side key down to the next. A press at either end stays on that page. \
         The call queues a full refresh and returns immediately. The text argument is required. Do not invent text.",
        gallery::WIDTH, gallery::HEIGHT
    );
    let bitmap_desc = alloc::format!(
        "Store and display one monochrome portrait bitmap. Size is {}x{} (stride {} bytes, {} bytes total). \
         Encoding: row-major, MSB is the leftmost pixel, 1 = white, 0 = black. \
         Pass the raw bytes as standard base64 in data. The panel stores {} bitmaps. \
         Omit index to append; pass index to overwrite that slot. replace=true clears the stored set first. \
         The call returns when the bitmap is stored and the refresh is queued.",
        gallery::WIDTH,
        gallery::HEIGHT,
        gallery::STRIDE,
        gallery::BYTES,
        gallery::CAPACITY
    );
    json!({
        "tools": [
            {
                "name": "show_text",
                "description": text_desc,
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "text": {
                            "type": "string",
                            "minLength": 1,
                            "maxLength": TEXT_MAX,
                            "description": "Exact text to show. Newlines are kept."
                        }
                    },
                    "required": ["text"],
                    "additionalProperties": false
                }
            },
            {
                "name": "show_bitmap",
                "description": bitmap_desc,
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "data": {
                            "type": "string",
                            "minLength": 1,
                            "description": alloc::format!(
                                "Standard base64 of exactly {} raw bytes ({}x{}, stride {}).",
                                gallery::BYTES, gallery::WIDTH, gallery::HEIGHT, gallery::STRIDE
                            )
                        },
                        "index": {
                            "type": "integer",
                            "minimum": 0,
                            "maximum": gallery::CAPACITY - 1,
                            "description": "Slot to overwrite. Omit to append the next free slot."
                        },
                        "replace": {
                            "type": "boolean",
                            "description": "Drop previously stored pages before storing this one."
                        }
                    },
                    "required": ["data"],
                    "additionalProperties": false
                }
            },
            {
                "name": "panel_status",
                "description": "Read IP, refresh state, bitmap size, and how many bitmaps are stored. Does not change the panel.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            }
        ]
    })
}

async fn call_tool(req: &Value) -> Result<Value, String> {
    let name = req
        .pointer("/params/name")
        .and_then(Value::as_str)
        .unwrap_or("");
    match name {
        "show_text" => {
            let Some(text) = clean_text(tool_text(req)) else {
                return Err(
                    "Text is missing. Ask the user what to display on the X4, then call show_text with that text."
                        .into(),
                );
            };
            if text.chars().count() > TEXT_MAX {
                return Err(alloc::format!(
                    "Text is too long (max {TEXT_MAX} characters)."
                ));
            }
            info!("MCP show_text queued ({} chars)", text.chars().count());
            let pages = text::render_pages(&text);
            let count = pages.len();
            gallery::begin_set().await;
            for (index, page) in pages.iter().enumerate() {
                if let Err(msg) = gallery::store(Some(index), page).await {
                    return Err(msg.into());
                }
            }
            gallery::mark_shown(0).await;
            let label: String = text.chars().take(80).collect();
            jobs::enqueue(Job::Slot(0), &label).await;
            let cut = if count >= gallery::CAPACITY {
                " The note was cut at the page limit."
            } else {
                ""
            };
            Ok(json!({
                "content": [{
                    "type": "text",
                    "text": alloc::format!(
                        "Queued {count} page(s) on the X4 ({}x{} portrait). Side up is the previous page, side down is the next.{cut}",
                        gallery::WIDTH, gallery::HEIGHT
                    )
                }]
            }))
        }
        "show_bitmap" => {
            let Some(raw) = tool_data(req) else {
                return Err(
                    "Bitmap data is missing. Ask the user for a monochrome bitmap, then call show_bitmap with its base64 in data."
                        .into(),
                );
            };
            let bytes = decode_bitmap(raw)?;
            let index = bitmap_index(req)?;
            if tool_flag(req, "replace") {
                gallery::begin_set().await;
            }
            let slot = match gallery::store(index, &bytes).await {
                Ok(slot) => slot,
                Err("bitmap store is full") => {
                    return Err(alloc::format!(
                        "Bitmap store is full ({}/{}). Pass index to overwrite a slot.",
                        gallery::CAPACITY, gallery::CAPACITY
                    ));
                }
                Err("bitmap index is out of range") => {
                    return Err(alloc::format!(
                        "Bitmap index is out of range. Slots are 0..{}.",
                        gallery::CAPACITY - 1
                    ));
                }
                Err(msg) => return Err(msg.into()),
            };
            let info = gallery::info().await;
            info!(
                "MCP show_bitmap slot {slot} ({}/{})",
                info.count, info.capacity
            );
            jobs::enqueue(Job::Slot(slot), &alloc::format!("bitmap {slot}")).await;
            Ok(json!({
                "content": [{
                    "type": "text",
                    "text": alloc::format!(
                        "Stored bitmap in slot {slot} ({}/{}). Size {}x{}, stride {}, {} bytes, 1=white 0=black. Full refresh queued.",
                        info.count, info.capacity, gallery::WIDTH, gallery::HEIGHT, gallery::STRIDE, gallery::BYTES
                    )
                }]
            }))
        }
        "panel_status" => {
            let status = jobs::status().await;
            let info = gallery::info().await;
            let url = if status.ip.is_empty() {
                "http://<no-ip>/mcp".to_string()
            } else {
                alloc::format!("http://{}/mcp", status.ip)
            };
            let shown = info
                .shown
                .map(|n| n.to_string())
                .unwrap_or_else(|| "-".into());
            let text = alloc::format!(
                "url: {url}\norientation: portrait {}x{}\nrefreshing: {}\npending: {}\nlast: {}\nbitmap: {}x{} stride {} bytes {} (1=white, 0=black, MSB left)\nstored: {}/{}\nshown: {shown}",
                gallery::WIDTH,
                gallery::HEIGHT,
                status.refreshing,
                status.pending,
                status.last,
                gallery::WIDTH,
                gallery::HEIGHT,
                gallery::STRIDE,
                gallery::BYTES,
                info.count,
                info.capacity
            );
            Ok(json!({
                "content": [{"type": "text", "text": text}]
            }))
        }
        _ => Err("Unknown tool. Use show_text, show_bitmap, or panel_status.".into()),
    }
}

fn tool_flag(req: &Value, name: &str) -> bool {
    req.pointer(&alloc::format!("/params/arguments/{name}"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn tool_data(req: &Value) -> Option<&str> {
    req.pointer("/params/arguments/data")
        .and_then(Value::as_str)
}

fn bitmap_index(req: &Value) -> Result<Option<usize>, String> {
    let Some(value) = req.pointer("/params/arguments/index") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(index) = value.as_u64() else {
        return Err("index must be a non-negative integer.".into());
    };
    Ok(Some(index as usize))
}

fn decode_bitmap(data: &str) -> Result<Vec<u8>, String> {
    let cleaned: Vec<u8> = data
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(cleaned)
        .map_err(|_| "Bitmap data is not valid standard base64.".to_string())?;
    if bytes.len() != gallery::BYTES {
        return Err(alloc::format!(
            "Bitmap is {} bytes after base64. It must be exactly {} bytes ({}x{}, stride {}).",
            bytes.len(),
            gallery::BYTES,
            gallery::WIDTH,
            gallery::HEIGHT,
            gallery::STRIDE
        ));
    }
    Ok(bytes)
}

fn tool_text(req: &Value) -> Option<&str> {
    let args = req.pointer("/params/arguments")?;
    if let Some(s) = args.get("text").and_then(Value::as_str) {
        Some(s)
    } else if let Some(s) = args.as_str() {
        Some(s)
    } else {
        None
    }
}

fn clean_text(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let mut out = String::new();
    for c in raw.chars() {
        if c == '\n' || c == '\t' {
            out.push(if c == '\t' { ' ' } else { c });
        } else if !c.is_control() {
            out.push(c);
        }
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn issue_session() -> u32 {
    let id = SESSION_NONCE.fetch_add(1, Ordering::Relaxed);
    let id = if id == 0 { 1 } else { id };
    SESSION.store(id, Ordering::Relaxed);
    id
}

fn session_header() -> Option<u32> {
    let id = SESSION.load(Ordering::Relaxed);
    if id == 0 { None } else { Some(id) }
}

fn session_matches(headers: &str) -> bool {
    let current = SESSION.load(Ordering::Relaxed);
    if current == 0 {
        return false;
    }
    header_value(headers, "mcp-session-id")
        .and_then(parse_session)
        .is_some_and(|id| id == current)
}

fn parse_session(value: &str) -> Option<u32> {
    let value = value.trim();
    if value.len() > 8 || value.is_empty() {
        return None;
    }
    u32::from_str_radix(value, 16).ok()
}

fn check_protocol_header(headers: &str) -> Result<(), u16> {
    match header_value(headers, "mcp-protocol-version") {
        None => Ok(()),
        Some(v) if PROTOCOLS.contains(&v) => Ok(()),
        Some(_) => Err(400),
    }
}

fn rpc_ok(id: Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

fn rpc_err(id: Value, code: i32, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message}
    })
    .to_string()
}

async fn help_text() -> String {
    let status = jobs::status().await;
    let host = if status.ip.is_empty() {
        "<no-ip>"
    } else {
        status.ip.as_str()
    };
    alloc::format!(
        "VaultHomeDisp X4 MCP\nPOST http://{host}/mcp\ntools: show_text, show_bitmap, panel_status\nbitmap {}x{} x{} slots\n",
        gallery::WIDTH,
        gallery::HEIGHT,
        gallery::CAPACITY
    )
}

async fn read_http(socket: &mut TcpSocket<'_>, buf: &mut [u8]) -> Result<usize, &'static str> {
    let mut filled = 0;
    loop {
        if filled >= buf.len() {
            return Err("headers too large");
        }
        let n = socket
            .read(&mut buf[filled..])
            .await
            .map_err(|_| "read error")?;
        if n == 0 {
            return Ok(if filled == 0 { 0 } else { filled });
        }
        filled += n;
        let Some(header_end) = find_headers_end(&buf[..filled]) else {
            continue;
        };
        let header_str = core::str::from_utf8(&buf[..header_end]).unwrap_or("");
        let post = header_str.starts_with("POST ") || header_str.starts_with("PUT ");
        let Some(content_len) = content_length(header_str) else {
            if post {
                return Err("length required");
            }
            return Ok(header_end);
        };
        let total = header_end.saturating_add(content_len);
        if total > buf.len() {
            return Err("body too large");
        }
        while filled < total {
            let n = socket
                .read(&mut buf[filled..total])
                .await
                .map_err(|_| "read error")?;
            if n == 0 {
                return Err("truncated body");
            }
            filled += n;
        }
        return Ok(total);
    }
}

async fn write_http(
    socket: &mut TcpSocket<'_>,
    status: u16,
    ctype: Option<&str>,
    body: &[u8],
    session: Option<u32>,
) -> Result<(), &'static str> {
    write_http_proto(socket, status, ctype, body, session, PROTOCOL_DEFAULT).await
}

async fn write_http_proto(
    socket: &mut TcpSocket<'_>,
    status: u16,
    ctype: Option<&str>,
    body: &[u8],
    session: Option<u32>,
    protocol: &str,
) -> Result<(), &'static str> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Payload Too Large",
        _ => "OK",
    };
    let ctype_line = match ctype {
        Some(c) => alloc::format!("Content-Type: {c}\r\n"),
        None => String::new(),
    };
    let session_line = match session {
        Some(id) => {
            alloc::format!("Mcp-Session-Id: {id:08x}\r\nMCP-Protocol-Version: {protocol}\r\n")
        }
        None => String::new(),
    };
    let header = alloc::format!(
        "HTTP/1.1 {status} {reason}\r\n{ctype_line}{session_line}Content-Length: {}\r\nConnection: keep-alive\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: Content-Type, Accept, Mcp-Session-Id, MCP-Protocol-Version\r\nAccess-Control-Allow-Methods: POST, GET, DELETE, OPTIONS\r\n\r\n",
        body.len()
    );
    socket
        .write_all(header.as_bytes())
        .await
        .map_err(|_| "write error")?;
    if !body.is_empty() {
        socket.write_all(body).await.map_err(|_| "write error")?;
    }
    socket.flush().await.map_err(|_| "flush error")?;
    Ok(())
}

fn find_headers_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

fn content_length(headers: &str) -> Option<usize> {
    header_value(headers, "content-length")?.parse().ok()
}

fn split_http(req: &str) -> Option<(&str, &str, &str, &str)> {
    let header_end = req.find("\r\n\r\n")?;
    let (head, body) = req.split_at(header_end + 4);
    let line = head.lines().next()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    let path = parts.next()?.split('?').next()?;
    Some((method, path, head, body))
}

fn header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    for line in headers.split("\r\n") {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        if k.trim().eq_ignore_ascii_case(name) {
            return Some(v.trim());
        }
    }
    None
}

fn wants_close(headers: &str) -> bool {
    let conn = header_value(headers, "connection").unwrap_or("");
    if conn.eq_ignore_ascii_case("close") {
        return true;
    }
    let first = headers.lines().next().unwrap_or("");
    first.contains("HTTP/1.0") && !conn.eq_ignore_ascii_case("keep-alive")
}

fn accept_sse(headers: &str) -> bool {
    header_value(headers, "accept")
        .map(|v| v.to_ascii_lowercase().contains("text/event-stream"))
        .unwrap_or(false)
}
