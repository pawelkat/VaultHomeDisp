//! Basic markdown pages for `show_text`.
//!
//! Body text is Liberation Sans at 18 px. Headings are the same face, larger.
//! The glass is 1-bit, so a coverage of 96 or more becomes black. That keeps
//! the thin strokes of an outline font from dropping out.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gallery::{self, BYTES, HEIGHT, WIDTH};
use log::info;

const FONT_BYTES: &[u8] = include_bytes!("fonts/LiberationSans-Latin.ttf");

const MARGIN: usize = 20;
const TOP: usize = 18;
const FOOTER: usize = 36;
const BOTTOM: usize = HEIGHT - FOOTER;

const BODY: f32 = 18.0;
const H2: f32 = 22.0;
const H1: f32 = 26.0;
const INK: u8 = 96;

pub fn render(text: &str) -> Vec<u8> {
    render_pages(text).into_iter().next().unwrap_or_else(blank)
}

/// One framebuffer per page, at most [`gallery::CAPACITY`].
pub fn render_pages(markdown: &str) -> Vec<Vec<u8>> {
    info!("X4 loading text face");
    let face = fontdue::Font::from_bytes(
        FONT_BYTES,
        fontdue::FontSettings {
            scale: BODY,
            ..fontdue::FontSettings::default()
        },
    )
    .expect("Liberation Sans");
    info!("X4 text face ready");
    let mut canvas = Canvas::new(face);
    let source = strip_frontmatter(markdown);
    let mut saw_content = false;
    for raw in source.split('\n') {
        let line = raw.trim_end_matches('\r').trim();
        if line.is_empty() || line == "---" {
            if saw_content {
                canvas.gap(10);
            }
            continue;
        }
        let (text, px, indent) = classify(line);
        let text = plain(text);
        if text.is_empty() {
            continue;
        }
        saw_content = true;
        canvas.paragraph(&text, px, indent);
        if canvas.stopped {
            break;
        }
    }
    if !saw_content {
        canvas.paragraph("Empty note", BODY, 0);
    }
    let pages = canvas.finish();
    info!("X4 text laid out {} page(s)", pages.len());
    pages
}

fn classify(line: &str) -> (&str, f32, usize) {
    if let Some(text) = line.strip_prefix("# ") {
        (text.trim(), H1, 0)
    } else if let Some(text) = line.strip_prefix("## ") {
        (text.trim(), H2, 0)
    } else if let Some(text) = line.strip_prefix("### ") {
        (text.trim(), H2, 0)
    } else if let Some(text) = line.strip_prefix("- ") {
        (text.trim(), BODY, 22)
    } else if let Some(text) = line.strip_prefix("* ") {
        (text.trim(), BODY, 22)
    } else {
        (line, BODY, 0)
    }
}

fn strip_frontmatter(markdown: &str) -> &str {
    let trimmed = markdown.trim_start();
    let Some(rest) = trimmed.strip_prefix("---") else {
        return markdown;
    };
    let Some(end) = rest.find("\n---") else {
        return markdown;
    };
    rest[end + 4..].trim_start_matches(['\r', '\n'])
}

/// `[[Note]]` and `[[Note|label]]` become the visible name. Bold markers go.
fn plain(input: &str) -> String {
    let mut out = String::new();
    let mut rest = input;
    while !rest.is_empty() {
        if let Some(inner) = rest.strip_prefix("[[") {
            if let Some(end) = inner.find("]]") {
                let body = &inner[..end];
                let label = body.rsplit('|').next().unwrap_or(body);
                let label = label.split('#').next().unwrap_or(label).trim();
                out.push_str(label);
                rest = &inner[end + 2..];
                continue;
            }
        }
        if rest.starts_with("**") || rest.starts_with("__") {
            rest = &rest[2..];
            continue;
        }
        let ch = rest.chars().next().unwrap();
        if ch != '`' {
            out.push(fold(ch));
        }
        rest = &rest[ch.len_utf8()..];
    }
    out
}

fn fold(ch: char) -> char {
    match ch {
        '°' => 'o',
        '–' | '—' | '−' => '-',
        '‘' | '’' | '´' => '\'',
        '“' | '”' => '"',
        '\u{00a0}' => ' ',
        other => other,
    }
}

struct Glyph {
    xmin: i32,
    ymin: i32,
    width: usize,
    height: usize,
    advance: f32,
    bitmap: Vec<u8>,
}

struct Canvas {
    face: fontdue::Font,
    glyphs: BTreeMap<(char, u16), Glyph>,
    pages: Vec<Vec<u8>>,
    frame: Vec<u8>,
    y: usize,
    stopped: bool,
}

impl Canvas {
    fn new(face: fontdue::Font) -> Self {
        Self {
            face,
            glyphs: BTreeMap::new(),
            pages: Vec::new(),
            frame: blank(),
            y: TOP,
            stopped: false,
        }
    }

    fn line_height(&self, px: f32) -> usize {
        let metrics = self
            .face
            .horizontal_line_metrics(px)
            .expect("horizontal metrics");
        ceil_usize(metrics.new_line_size).saturating_add(3)
    }

    fn ascent(&self, px: f32) -> i32 {
        let metrics = self
            .face
            .horizontal_line_metrics(px)
            .expect("horizontal metrics");
        ceil_i32(metrics.ascent)
    }

    fn gap(&mut self, px: usize) {
        if self.stopped || self.y <= TOP {
            return;
        }
        self.y = (self.y + px).min(BOTTOM);
    }

    fn paragraph(&mut self, text: &str, px: f32, indent: usize) {
        if self.stopped {
            return;
        }
        if px > BODY && self.y > TOP {
            self.gap(8);
        }
        let line_h = self.line_height(px);
        let left = (MARGIN + indent) as f32;
        let right = (WIDTH.saturating_sub(MARGIN)) as f32;
        let prefix = if indent > 0 { "- " } else { "" };
        let mut x = left;
        let mut open = false;
        let mut words = prefix_words(prefix, text);
        while let Some(word) = words.next() {
            if self.stopped {
                return;
            }
            let width = self.measure(word, px);
            let room = right - left;
            if open && x + width > right && width <= room {
                self.y += line_h;
                x = left;
                open = false;
            }
            if !open {
                if !self.break_line(line_h) {
                    return;
                }
                open = true;
            }
            for ch in word.chars() {
                let advance = self.face.metrics(ch, px).advance_width;
                if x + advance > right && x > left {
                    self.y += line_h;
                    x = left;
                    if !self.break_line(line_h) {
                        return;
                    }
                }
                let baseline = self.y as i32 + self.ascent(px);
                x += self.draw_char(ch, x, baseline, px);
            }
        }
        self.y += line_h;
    }

    fn measure(&self, text: &str, px: f32) -> f32 {
        text.chars()
            .map(|ch| self.face.metrics(ch, px).advance_width)
            .sum()
    }

    fn break_line(&mut self, line_h: usize) -> bool {
        if self.y + line_h <= BOTTOM {
            return true;
        }
        if self.pages.len() + 1 >= gallery::CAPACITY {
            self.stopped = true;
            return false;
        }
        let finished = core::mem::replace(&mut self.frame, blank());
        self.pages.push(finished);
        self.y = TOP;
        true
    }

    fn finish(mut self) -> Vec<Vec<u8>> {
        let used = self.frame.iter().any(|byte| *byte != 0xFF);
        if used || self.pages.is_empty() {
            self.pages.push(core::mem::take(&mut self.frame));
        }
        let count = self.pages.len();
        if count > 1 {
            let labels: Vec<String> = (1..=count)
                .map(|index| alloc::format!("{index}/{count}"))
                .collect();
            let mut pages = core::mem::take(&mut self.pages);
            for (page, label) in pages.iter_mut().zip(labels.iter()) {
                let width = self.measure(label, BODY);
                let mut x = (WIDTH.saturating_sub(MARGIN)) as f32 - width;
                let baseline = (HEIGHT - 14) as i32;
                for ch in label.chars() {
                    x += self.draw_char_on(page, ch, x, baseline, BODY);
                }
            }
            return pages;
        }
        self.pages
    }

    fn draw_char(&mut self, ch: char, x: f32, baseline: i32, px: f32) -> f32 {
        let mut frame = core::mem::take(&mut self.frame);
        let advance = self.draw_char_on(&mut frame, ch, x, baseline, px);
        self.frame = frame;
        advance
    }

    fn draw_char_on(&mut self, frame: &mut [u8], ch: char, x: f32, baseline: i32, px: f32) -> f32 {
        let key = (ch, px as u16);
        if !self.glyphs.contains_key(&key) {
            let (metrics, bitmap) = self.face.rasterize(ch, px);
            self.glyphs.insert(
                key,
                Glyph {
                    xmin: metrics.xmin,
                    ymin: metrics.ymin,
                    width: metrics.width,
                    height: metrics.height,
                    advance: metrics.advance_width,
                    bitmap,
                },
            );
        }
        let glyph = self.glyphs.get(&key).expect("glyph just stored");
        let origin_x = round_i32(x) + glyph.xmin;
        let top = baseline - glyph.ymin - glyph.height as i32;
        let width = glyph.width;
        for row in 0..glyph.height {
            for col in 0..width {
                if glyph.bitmap[row * width + col] < INK {
                    continue;
                }
                set_black(frame, origin_x + col as i32, top + row as i32);
            }
        }
        glyph.advance
    }
}

struct Words<'a> {
    prefix: &'a str,
    rest: &'a str,
}

fn prefix_words<'a>(prefix: &'a str, text: &'a str) -> Words<'a> {
    Words { prefix, rest: text }
}

impl<'a> Iterator for Words<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.prefix.is_empty() {
            let word = self.prefix;
            self.prefix = "";
            return Some(word);
        }
        if self.rest.is_empty() {
            return None;
        }
        let end = self
            .rest
            .find(' ')
            .map(|index| index + 1)
            .unwrap_or(self.rest.len());
        let word = &self.rest[..end];
        self.rest = &self.rest[end..];
        if word.trim().is_empty() { self.next() } else { Some(word) }
    }
}

fn round_i32(value: f32) -> i32 {
    (value + if value >= 0.0 { 0.5 } else { -0.5 }) as i32
}

fn ceil_usize(value: f32) -> usize {
    let whole = value as usize;
    if value > whole as f32 { whole + 1 } else { whole }
}

fn ceil_i32(value: f32) -> i32 {
    let whole = value as i32;
    if value > whole as f32 { whole + 1 } else { whole }
}

fn blank() -> Vec<u8> {
    alloc::vec![0xFFu8; BYTES]
}

fn set_black(frame: &mut [u8], x: i32, y: i32) {
    if x < 0 || y < 0 || x >= WIDTH as i32 || y >= HEIGHT as i32 {
        return;
    }
    let x = x as usize;
    let y = y as usize;
    let index = y * (WIDTH / 8) + x / 8;
    frame[index] &= !(0x80 >> (x % 8));
}
