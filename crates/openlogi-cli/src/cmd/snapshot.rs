//! `openlogi snapshot` — grab one frame from a Logitech webcam to a PNG.
//!
//! Exercises the `openlogi-camera` capture path (the same primitive the GUI
//! preview uses). Capturing needs Camera permission; from this unbundled CLI
//! macOS may deny access (no `NSCameraUsageDescription`), which is reported
//! rather than fatal.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use clap::Args;

#[derive(Debug, Args)]
pub struct SnapshotArgs {
    /// Output PNG path.
    #[arg(default_value = "snapshot.png")]
    pub path: String,
    /// Capture from the camera with this unique id (default: first Logitech).
    #[arg(long)]
    pub camera: Option<String>,
}

pub fn run(args: SnapshotArgs) -> Result<()> {
    let unique_id = match args.camera {
        Some(id) => id,
        None => openlogi_camera::enumerate_cameras()
            .into_iter()
            .next()
            .map(|camera| camera.unique_id)
            .ok_or_else(|| anyhow!("no Logitech camera found"))?,
    };

    println!("capturing one frame from {unique_id} …");
    let frame = openlogi_camera::capture_frame(&unique_id, Duration::from_secs(5))
        .map_err(|e| anyhow!("{e}"))?;
    // Frames are stored BGRA (gpui's order); PNG wants RGBA, so swap R/B once.
    let mut rgba = frame.bgra;
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    write_png(&args.path, frame.width, frame.height, &rgba)
        .with_context(|| format!("writing {}", args.path))?;
    println!("wrote {}x{} → {}", frame.width, frame.height, args.path);
    Ok(())
}

fn write_png(path: &str, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
    let file = std::fs::File::create(path)?;
    let writer = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()?
        .write_image_data(rgba)
        .context("encoding PNG")
}
