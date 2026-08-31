//! Native application icons shared by OpenLogi's GPUI processes.

use std::sync::Arc;

/// Render the Finder icon for the exact macOS application bundle at `path`.
///
/// The lookup is blocking and must run off the render path.
#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "AppKit allocates the offscreen bitmap and exposes its pixel buffer through a raw pointer"
)]
#[must_use]
pub fn application_icon(path: &str, size: u32) -> Option<Arc<gpui::RenderImage>> {
    use std::path::Path;

    use objc2::AnyThread as _;
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::{
        NSBitmapFormat, NSBitmapImageRep, NSCompositingOperation, NSDeviceRGBColorSpace,
        NSGraphicsContext, NSWorkspace,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

    let expanded = shellexpand::tilde(path);
    let path = Path::new(expanded.as_ref());
    if !path.is_dir()
        || !path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
    {
        return None;
    }
    let edge = isize::try_from(size).ok().filter(|edge| *edge > 0)?;

    autoreleasepool(|_| {
        let path = NSString::from_str(path.to_string_lossy().as_ref());
        let icon = NSWorkspace::sharedWorkspace().iconForFile(&path);

        // SAFETY: null data planes ask AppKit to allocate a buffer sized by
        // the self-consistent 8-bit RGBA layout passed here.
        let bitmap = unsafe {
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(),
                std::ptr::null_mut(),
                edge,
                edge,
                8,
                4,
                true,
                false,
                NSDeviceRGBColorSpace,
                0,
                0,
            )?
        };
        let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&bitmap)?;
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&context));
        icon.drawInRect_fromRect_operation_fraction(
            NSRect::new(
                NSPoint::new(0., 0.),
                NSSize::new(f64::from(size), f64::from(size)),
            ),
            NSRect::ZERO,
            NSCompositingOperation::SourceOver,
            1.0,
        );
        NSGraphicsContext::restoreGraphicsState_class();

        let unexpected = NSBitmapFormat::AlphaFirst
            | NSBitmapFormat::AlphaNonpremultiplied
            | NSBitmapFormat::FloatingPointSamples;
        if bitmap.bitmapFormat().intersects(unexpected) {
            return None;
        }

        let pixel_edge = usize::try_from(size).ok()?;
        let row_bytes = pixel_edge.checked_mul(4)?;
        let source_stride = usize::try_from(bitmap.bytesPerRow()).ok()?;
        let source = bitmap.bitmapData();
        if source.is_null() || source_stride < row_bytes {
            return None;
        }
        let mut rgba = vec![0_u8; row_bytes.checked_mul(pixel_edge)?];
        for (row, target) in rgba.chunks_exact_mut(row_bytes).enumerate() {
            // SAFETY: `source` is the bitmap-owned buffer, `row` is below its
            // height, and each source row contains at least `row_bytes` bytes.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    source.add(row.checked_mul(source_stride)?),
                    target.as_mut_ptr(),
                    row_bytes,
                );
            }
        }
        unpremultiply(&mut rgba);
        crate::image::render_image_from_rgba(size, size, rgba)
    })
}

/// Native application icons are not implemented away from macOS.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn application_icon(_path: &str, _size: u32) -> Option<Arc<gpui::RenderImage>> {
    None
}

#[cfg(target_os = "macos")]
fn unpremultiply(rgba: &mut [u8]) {
    let (pixels, _) = rgba.as_chunks_mut::<4>();
    for pixel in pixels {
        let alpha = u32::from(pixel[3]);
        if alpha == 0 || alpha == 255 {
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel =
                u8::try_from((u32::from(*channel) * 255 + alpha / 2) / alpha).unwrap_or(u8::MAX);
        }
    }
}
