//! Raster-image adapters shared by OpenLogi's GPUI processes.

use std::sync::Arc;

/// Wrap straight-alpha RGBA pixels as a GPUI texture.
///
/// [`gpui::RenderImage`] frames hold BGRA, so red and blue swap in place before
/// the buffer is consumed.
#[must_use]
pub fn render_image_from_rgba(
    width: u32,
    height: u32,
    mut rgba: Vec<u8>,
) -> Option<Arc<gpui::RenderImage>> {
    let (pixels, _) = rgba.as_chunks_mut::<4>();
    for pixel in pixels {
        pixel.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(width, height, rgba)?;
    Some(Arc::new(gpui::RenderImage::new(vec![image::Frame::new(
        buffer,
    )])))
}
