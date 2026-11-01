//! Portrait text for `show_text`. One 5×7 glyph, scaled, 1 = white.

use alloc::vec::Vec;

use crate::font;
use crate::gallery::{BYTES, HEIGHT, WIDTH};

pub fn render(text: &str) -> Vec<u8> {
    let mut frame = alloc::vec![0xFFu8; BYTES];
    let scale = 3usize;
    let advance = 5 * scale + 2;
    let line_h = 7 * scale + 6;
    let left = 12usize;
    let right = WIDTH.saturating_sub(12);
    let mut x = left;
    let mut y = 16usize;

    for raw_line in text.split('\n') {
        let mut rest = raw_line;
        if rest.is_empty() {
            y += line_h;
            continue;
        }
        while !rest.is_empty() {
            let word_end = rest.find(' ').map(|i| i + 1).unwrap_or(rest.len());
            let word = &rest[..word_end];
            let width = word.chars().count() * advance;
            if x != left && x.saturating_add(width) > right {
                x = left;
                y += line_h;
            }
            for ch in word.chars() {
                if y + 7 * scale >= HEIGHT.saturating_sub(8) {
                    return frame;
                }
                if x + advance > right {
                    x = left;
                    y += line_h;
                }
                draw_char(&mut frame, ch, x, y, scale);
                x += advance;
            }
            rest = &rest[word_end..];
        }
        x = left;
        y += line_h;
    }
    frame
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
            fill(
                frame,
                x0 + col * scale,
                y0 + row * scale,
                scale,
                scale,
            );
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
