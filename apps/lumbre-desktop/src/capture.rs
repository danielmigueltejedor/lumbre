//! Saves a picture of the window, for documentation and visual review.
//!
//! `LUMBRE_CAPTURE=/path/shot.bmp lumbre` renders the interface as it really is (live daemon
//! state included), writes one frame and exits. `LUMBRE_CAPTURE_VIEW=models|privacy` picks the
//! view. Not used in normal operation.

use std::io::Write;
use std::path::PathBuf;

use eframe::egui::{ColorImage, Context, ViewportCommand};

pub struct Capture {
    target: Option<PathBuf>,
    frames: u32,
    opened: bool,
}

impl Capture {
    /// Frames to wait first, so the first status and lists have arrived and been laid out.
    const SETTLE_FRAMES: u32 = 90;

    pub fn from_env() -> Self {
        Self { target: std::env::var_os("LUMBRE_CAPTURE").map(PathBuf::from), frames: 0, opened: false }
    }

    pub fn view() -> Option<String> {
        std::env::var("LUMBRE_CAPTURE_VIEW").ok()
    }

    pub fn wants_conversation(&self) -> bool {
        self.target.is_some() && !self.opened
    }

    pub fn conversation_opened(&mut self) {
        self.opened = true;
    }

    pub fn tick(&mut self, ctx: &Context) {
        let Some(target) = &self.target else { return };
        self.frames += 1;
        ctx.request_repaint();
        if self.frames != Self::SETTLE_FRAMES {
            return;
        }
        let target = target.clone();
        let closer = ctx.clone();
        ctx.request_screenshot(move |image| {
            if let Err(error) = write_bmp(&target, &image) {
                eprintln!("lumbre: could not write {}: {error}", target.display());
            }
            closer.send_viewport_cmd(ViewportCommand::Close);
        });
    }
}

/// An uncompressed 24-bit BMP: trivial to write and every image tool reads it.
fn write_bmp(path: &std::path::Path, image: &ColorImage) -> std::io::Result<()> {
    let [width, height] = image.size;
    let row = (width * 3).div_ceil(4) * 4;
    let size = 54 + row * height;
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(height as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    // Rows are stored bottom-up, pixels as BGR.
    for y in (0..height).rev() {
        for pixel in &image.pixels[y * width..(y + 1) * width] {
            out.extend_from_slice(&[pixel.b(), pixel.g(), pixel.r()]);
        }
        out.resize(out.len() + row - width * 3, 0);
    }
    std::fs::File::create(path)?.write_all(&out)
}
