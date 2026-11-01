//! Basic markdown pages for `show_text`.
//!
//! Headings, bullets, paragraphs, and `[[wikilinks]]` (shown as their visible
//! name). A link is not selectable. Pages are 480×800, 1 = white.

use alloc::string::String;
use alloc::vec::Vec;

use crate::font;
use crate::gallery::{self, BYTES, HEIGHT, WIDTH};

const MARGIN: usize = 16;
const TOP: usize = 16;
const FOOTER: usize = 28;
const BOTTOM: usize = HEIGHT - FOOTER;

const BODY: usize = 2;
const H2: usize = 3;
const H1: usize = 4;

pub fn render(text: &str) -> Vec<u8> {
    render_pages(text).into_iter().next().unwrap_or_else(blank)
}

/// One framebuffer per page, at most [`gallery::CAPACITY`].
pub fn render_pages(markdown: &str) -> Vec<Vec<u8>> {
    let mut canvas = Canvas::new();
    let source = strip_frontmatter(markdown);
    let mut saw_content = false;
    for raw in source.split('\n') {
        let line = raw.trim_end_matches('\r').trim();
        if line.is_empty() || line == "---" {
            if saw_content {
                canvas.gap(8);
            }
            continue;
        }
        let (text, scale, indent) = classify(line);
        let text = plain(text);
        if text.is_empty() {
            continue;
        }
        saw_content = true;
        canvas.paragraph(&text, scale, indent);
        if canvas.stopped {
            break;
        }
    }
    if !saw_content {
        canvas.paragraph("Empty note", BODY, 0);
    }
    canvas.finish()
}

fn classify(line: &str) -> (&str, usize, usize) {
    if let Some(text) = line.strip_prefix("# ") {
        (text.trim(), H1, 0)
    } else if let Some(text) = line.strip_prefix("## ") {
        (text.trim(), H2, 0)
    } else if let Some(text) = line.strip_prefix("### ") {
        (text.trim(), H2, 0)
    } else if let Some(text) = line.strip_prefix("- ") {
        (text.trim(), BODY, 18)
    } else if let Some(text) = line.strip_prefix("* ") {
        (text.trim(), BODY, 18)
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

struct Canvas {
    pages: Vec<Vec<u8>>,
    frame: Vec<u8>,
    y: usize,
    stopped: bool,
}

impl Canvas {
    fn new() -> Self {
        Self {
            pages: Vec::new(),
            frame: blank(),
            y: TOP,
            stopped: false,
        }
    }

    fn gap(&mut self, px: usize) {
        if self.stopped || self.y <= TOP {
            return;
        }
        self.y = (self.y + px).min(BOTTOM);
    }

    fn paragraph(&mut self, text: &str, scale: usize, indent: usize) {
        if self.stopped {
            return;
        }
        if scale > BODY && self.y > TOP {
            self.gap(8);
        }
        let advance = 5 * scale + 2;
        let line_h = 7 * scale + 6;
        let left = MARGIN + indent;
        let right = WIDTH.saturating_sub(MARGIN);
        let prefix = if indent > 0 { "- " } else { "" };
        let mut x = left;
        let mut open = false;
        let mut words = prefix_words(prefix, text);
        while let Some(word) = words.next() {
            if self.stopped {
                return;
            }
            let width = word.chars().count() * advance;
            if open && x + width > right && width <= right.saturating_sub(left) {
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
                if x + advance > right {
                    self.y += line_h;
                    x = left;
                    if !self.break_line(line_h) {
                        return;
                    }
                }
                draw_char(&mut self.frame, ch, x, self.y, scale);
                x += advance;
            }
        }
        self.y += line_h;
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
            self.pages.push(self.frame);
            self.frame = Vec::new();
        }
        let count = self.pages.len();
        if count > 1 {
            for (index, page) in self.pages.iter_mut().enumerate() {
                let label = alloc::format!("{}/{}", index + 1, count);
                stamp_footer(page, &label);
            }
        }
        self.pages
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
        let end = self.rest.find(' ').map(|index| index + 1).unwrap_or(self.rest.len());
        let word = &self.rest[..end];
        self.rest = &self.rest[end..];
        if word.trim().is_empty() {
            self.next()
        } else {
            Some(word)
        }
    }
}

fn blank() -> Vec<u8> {
    alloc::vec![0xFFu8; BYTES]
}

fn stamp_footer(frame: &mut [u8], label: &str) {
    let scale = BODY;
    let advance = 5 * scale + 2;
    let width = label.chars().count() * advance;
    let x = WIDTH.saturating_sub(MARGIN + width);
    let y = HEIGHT - MARGIN - 7 * scale;
    for (index, ch) in label.chars().enumerate() {
        draw_char(frame, ch, x + index * advance, y, scale);
    }
}

fn draw_char(frame: &mut [u8], ch: char, x0: usize, y0: usize, scale: usize) {
    let Some(columns) = font::columns(ch) else {
        return;
    };
    for (col, bits) in columns.iter().enumerate() {
        for row in 0..7 {
            if bits & (1 << row) == 0 {
                continue;
            }
            fill(frame, x0 + col * scale, y0 + row * scale, scale, scale);
        }
    }
}

fn fill(frame: &mut [u8], x0: usize, y0: usize, w: usize, h: usize) {
    let stride = WIDTH / 8;
    for y in y0..y0 + h {
        if y >= HEIGHT {
            break;
        }
        for x in x0..x0 + w {
            if x >= WIDTH {
                break;
            }
            let index = y * stride + x / 8;
            frame[index] &= !(0x80 >> (x % 8));
        }
    }
}
