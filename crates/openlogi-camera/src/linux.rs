//! V4L2 device discovery on Linux.
//!
//! A UVC camera exposes several `/dev/video*` nodes — one for capture, plus a
//! metadata node carrying UVC timing data. `VIDIOC_QUERYCAP`'s `capabilities`
//! field reports the *union* across the physical device's nodes, so it reads
//! `VIDEO_CAPTURE` on the metadata node too; the `v4l` crate doesn't surface
//! the per-node `device_caps`. Nodes are therefore classified by whether
//! `VIDIOC_ENUM_FMT` yields any capture format, which only the capture node
//! does.

use std::fs;
use std::path::{Path, PathBuf};

use v4l::frameinterval::FrameIntervalEnum;
use v4l::video::Capture;
use v4l::{Device, FourCC};

use crate::Camera;

/// Where the kernel lists V4L2 nodes, one directory per `/dev/video*`.
const SYSFS_V4L: &str = "/sys/class/video4linux";

/// Stable-by-serial symlink farm `udev` maintains for V4L2 nodes.
const BY_ID_DIR: &str = "/dev/v4l/by-id";

/// A discovered capture node and the USB identity behind it.
pub(crate) struct Node {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) vendor_id: u16,
    pub(crate) product_id: u16,
    /// USB `iSerialNumber` from sysfs when the device reports one.
    pub(crate) serial_number: Option<String>,
    /// Canonicalized sysfs directory of the *USB device* (not interface)
    /// behind this node — shared by every capture node the same physical
    /// device exposes. See [`cameras`].
    usb_device: PathBuf,
}

/// Enumerate every V4L2 capture node, newest-first by node index.
///
/// Non-USB devices (virtual cameras, loopback nodes) have no `idVendor` in
/// sysfs and are skipped — they can't be attributed to a vendor, so the
/// Logitech filter in [`crate::enumerate_cameras`] couldn't judge them anyway.
pub(crate) fn nodes() -> Vec<Node> {
    let Ok(entries) = fs::read_dir(SYSFS_V4L) else {
        return Vec::new();
    };

    let mut nodes: Vec<Node> = entries
        .flatten()
        .filter_map(|entry| {
            let sysfs = entry.path();
            let dev_path = PathBuf::from("/dev").join(entry.file_name());
            let usb_device = usb_device_sysfs(&sysfs)?;
            let (vendor_id, product_id) = usb_ids(&usb_device)?;
            if !is_capture_node(&dev_path) {
                return None;
            }
            Some(Node {
                name: read_trimmed(&sysfs.join("name"))
                    .unwrap_or_else(|| dev_path.display().to_string()),
                path: dev_path,
                vendor_id,
                product_id,
                serial_number: usb_serial(&usb_device),
                usb_device,
            })
        })
        .collect();

    nodes.sort_by(|a, b| a.path.cmp(&b.path));
    nodes
}

/// Resolve a [`Camera::unique_id`] back to the `/dev/video*` node it names.
///
/// Ids are `by-id` symlinks when udev provides one, so this canonicalizes
/// before comparing — a `by-id` path and its `/dev/videoN` target must resolve
/// to the same node.
pub(crate) fn node_for_unique_id(unique_id: &str) -> Option<PathBuf> {
    let target = fs::canonicalize(unique_id).ok()?;
    nodes()
        .into_iter()
        .find(|node| fs::canonicalize(&node.path).is_ok_and(|p| p == target))
        .map(|node| node.path)
}

/// Build the [`Camera`] view of a node, including its largest frame size and
/// highest frame rate. Format probing is metadata-only — `VIDIOC_ENUM_*`
/// never starts a stream, so this costs no LED and needs no permission beyond
/// opening the node.
pub(crate) fn describe(node: &Node) -> Camera {
    let (max_resolution, max_fps) =
        Device::with_path(&node.path).map_or((None, None), |device| max_format(&device));

    Camera {
        name: node.name.clone(),
        unique_id: unique_id_for(&node.path),
        serial_number: node.serial_number.clone(),
        vendor_id: node.vendor_id,
        product_id: node.product_id,
        max_resolution,
        max_fps,
    }
}

/// One [`Camera`] per physical USB device.
///
/// A UVC camera can expose more than one *streaming* interface — e.g. the
/// Brio's second, low-resolution node feeding its IR sensor for Windows
/// Hello — and each gets its own `/dev/videoN` capture node just like the
/// main color sensor does (issue #1191). Grouping by the shared USB device
/// directory and keeping only the highest-resolution node per group turns
/// that back into one listing entry per physical camera; `node_for_unique_id`
/// still resolves every node individually, so a secondary node stays
/// controllable if some other path ever needs it.
pub(crate) fn cameras() -> Vec<Camera> {
    let described = nodes().into_iter().map(|node| {
        let is_color = Device::with_path(&node.path).is_ok_and(|device| has_color_format(&device));
        (node.usb_device.clone(), describe(&node), is_color)
    });
    merge_by_usb_device(described)
}

/// Collapse `(usb_device, Camera, is_color)` triples to one `Camera` per
/// distinct `usb_device`, and otherwise preserving first-seen order.
///
/// A node's resolution is `None` when its primary format only ever reported
/// stepwise/continuous frame sizes, or enumeration failed outright — that is
/// *unknown*, not "0x0". Ranking it below any node with a discrete size would
/// let a tiny-but-known secondary/IR node (see [`cameras`]) outrank an
/// unmeasured primary sensor and steal its `unique_id`. So resolution only
/// ever decides the winner when both sides are known.
///
/// When resolution can't decide (either side unknown, or a tie), `is_color`
/// breaks it instead: a node that reports at least one non-monochrome pixel
/// format (see [`has_color_format`]) wins over a mono-only IR/depth node,
/// regardless of which `/dev/videoN` enumerated first — node numbering order
/// is not a reliable signal (issue: an IR node like `/dev/video10` can sort
/// before the color node `/dev/video2`). Only when neither resolution nor
/// color-capability can decide does the first-seen node keep its place, same
/// as an exact tie.
fn merge_by_usb_device(nodes: impl IntoIterator<Item = (PathBuf, Camera, bool)>) -> Vec<Camera> {
    let mut by_device: Vec<(PathBuf, Camera, bool)> = Vec::new();
    for (usb_device, camera, is_color) in nodes {
        match by_device.iter_mut().find(|(dev, _, _)| *dev == usb_device) {
            Some((_, best, best_is_color)) => {
                let by_resolution = match (camera.max_resolution, best.max_resolution) {
                    (Some(candidate), Some(current)) => {
                        Some(resolution_area(candidate) > resolution_area(current))
                    }
                    _ => None,
                };
                let candidate_wins = by_resolution.unwrap_or(is_color && !*best_is_color);
                if candidate_wins {
                    *best = camera;
                    *best_is_color = is_color;
                }
            }
            None => by_device.push((usb_device, camera, is_color)),
        }
    }
    by_device.into_iter().map(|(_, camera, _)| camera).collect()
}

/// Whether `device` reports at least one pixel format that isn't a
/// known monochrome-only V4L2 format.
///
/// UVC webcams with a secondary IR/depth sensor (e.g. the Brio's Windows
/// Hello node, issue #1191) expose it as a plain capture node just like the
/// primary color sensor, so it can't be told apart by `/dev/videoN` order or
/// resolution alone. IR sensors report single-channel formats (`GREY`/`Y8`,
/// `Y10`, `Y12`, `Y16`) where the color sensor reports YUV/RGB/compressed
/// formats, so this is checked instead of relying on enumeration order. A
/// node whose format list can't be read is *not* claimed to be a color node.
fn has_color_format(device: &Device) -> bool {
    let Ok(formats) = device.enum_formats() else {
        return false;
    };
    formats
        .iter()
        .any(|format| !is_monochrome_fourcc(format.fourcc))
}

/// Whether `fourcc` names a known monochrome-only V4L2 pixel format (as
/// opposed to a YUV/RGB/Bayer/compressed one carrying color information).
fn is_monochrome_fourcc(fourcc: FourCC) -> bool {
    matches!(
        &fourcc.repr,
        b"GREY" | b"Y8  " | b"Y10 " | b"Y12 " | b"Y16 "
    )
}

/// Pixel count of a resolution, for comparing which capture node is the
/// primary sensor.
fn resolution_area(resolution: (u32, u32)) -> u64 {
    let (w, h) = resolution;
    u64::from(w) * u64::from(h)
}

/// The `by-id` symlink for `path` when udev created one (it embeds the USB
/// serial, so it survives replugging into another port), else the raw node
/// path. Either way it round-trips through [`node_for_unique_id`].
fn unique_id_for(path: &Path) -> String {
    let canonical = fs::canonicalize(path).ok();
    let by_id = fs::read_dir(BY_ID_DIR).ok().and_then(|entries| {
        entries
            .flatten()
            .map(|entry| entry.path())
            .find(|link| fs::canonicalize(link).ok() == canonical)
    });
    by_id
        .unwrap_or_else(|| path.to_path_buf())
        .display()
        .to_string()
}

/// Read `idVendor`/`idProduct` from the USB device directory.
fn usb_ids(usb: &Path) -> Option<(u16, u16)> {
    let vendor = read_trimmed(&usb.join("idVendor"))?;
    let product = read_trimmed(&usb.join("idProduct"))?;
    Some((
        u16::from_str_radix(&vendor, 16).ok()?,
        u16::from_str_radix(&product, 16).ok()?,
    ))
}

/// USB `iSerialNumber` from the USB device directory, when present and
/// non-empty.
fn usb_serial(usb: &Path) -> Option<String> {
    let serial = read_trimmed(&usb.join("serial"))?;
    let serial = serial.trim();
    // Kernel placeholder when the descriptor has no iSerialNumber.
    if serial.is_empty() || serial == "0" {
        return None;
    }
    Some(serial.to_string())
}

/// Sysfs directory of the USB *device* behind a V4L2 node (parent of the
/// interface entry at `<sysfs>/device`).
fn usb_device_sysfs(sysfs: &Path) -> Option<PathBuf> {
    fs::canonicalize(sysfs.join("device").join("..")).ok()
}

/// Whether the node serves video capture, i.e. enumerates at least one capture
/// format. Metadata nodes open fine but enumerate none.
fn is_capture_node(dev_path: &Path) -> bool {
    Device::with_path(dev_path)
        .and_then(|device| device.enum_formats())
        .is_ok_and(|formats| !formats.is_empty())
}

/// Largest frame size across all formats, and the highest frame rate offered
/// at any size. Both are `None` when the driver reports only stepwise or
/// continuous ranges, which carry no single "max" worth showing.
fn max_format(device: &Device) -> (Option<(u32, u32)>, Option<u32>) {
    let Ok(formats) = device.enum_formats() else {
        return (None, None);
    };

    let mut max_resolution: Option<(u32, u32)> = None;
    let mut max_fps: Option<u32> = None;

    for format in formats {
        let Ok(sizes) = device.enum_framesizes(format.fourcc) else {
            continue;
        };
        for size in sizes {
            for discrete in size.size.to_discrete() {
                let candidate = (discrete.width, discrete.height);
                if max_resolution.is_none_or(|(w, h)| {
                    u64::from(candidate.0) * u64::from(candidate.1) > u64::from(w) * u64::from(h)
                }) {
                    max_resolution = Some(candidate);
                }
                if let Some(fps) = max_discrete_fps(device, format.fourcc, candidate) {
                    max_fps = Some(max_fps.map_or(fps, |best: u32| best.max(fps)));
                }
            }
        }
    }

    (max_resolution, max_fps)
}

/// Highest discrete frame rate the driver offers for one format and size.
///
/// Intervals are periods (seconds per frame), so the highest rate is the
/// smallest interval. Stepwise/continuous ranges are skipped — they describe a
/// span rather than an offered rate — as are zero-numerator entries, which
/// would divide by zero.
fn max_discrete_fps(device: &Device, fourcc: FourCC, size: (u32, u32)) -> Option<u32> {
    let intervals = device.enum_frameintervals(fourcc, size.0, size.1).ok()?;
    intervals
        .into_iter()
        .filter_map(|interval| match interval.interval {
            FrameIntervalEnum::Discrete(fraction) if fraction.numerator > 0 => {
                Some(fraction.denominator / fraction.numerator)
            }
            _ => None,
        })
        .max()
}

/// Read a sysfs attribute, trimming the trailing newline the kernel appends.
fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera(name: &str, max_resolution: Option<(u32, u32)>) -> Camera {
        Camera {
            name: name.to_string(),
            unique_id: name.to_string(),
            serial_number: Some("5091F273".to_string()),
            vendor_id: 0x046d,
            product_id: 0x085e,
            max_resolution,
            max_fps: None,
        }
    }

    #[test]
    fn brio_ir_node_collapses_into_the_main_capture_node() {
        // Reproduces issue #1191: the Brio's two capture-capable /dev/videoN
        // nodes (main sensor + IR sensor for Windows Hello) share one USB
        // device directory and must collapse to a single listing entry.
        let usb_device = PathBuf::from("/sys/devices/usb1/1-1");
        let main = camera("Logitech BRIO", Some((4096, 2160)));
        let ir = camera("Logitech BRIO", Some((340, 340)));

        let cameras = merge_by_usb_device([
            (usb_device.clone(), main.clone(), true),
            (usb_device, ir, false),
        ]);

        assert_eq!(cameras, vec![main]);
    }

    #[test]
    fn distinct_usb_devices_stay_separate() {
        let one = camera("Logitech BRIO", Some((4096, 2160)));
        let two = camera("Logitech StreamCam", Some((1920, 1080)));

        let cameras = merge_by_usb_device([
            (PathBuf::from("/sys/devices/usb1/1-1"), one.clone(), true),
            (PathBuf::from("/sys/devices/usb1/1-2"), two.clone(), true),
        ]);

        assert_eq!(cameras, vec![one, two]);
    }

    #[test]
    fn resolution_area_compares_pixel_counts() {
        assert_eq!(resolution_area((340, 340)), 340 * 340);
        assert!(resolution_area((4096, 2160)) > resolution_area((340, 340)));
    }

    #[test]
    fn unknown_resolution_does_not_lose_to_a_known_smaller_node() {
        // Reproduces the failure mode from the #1234 review: if the primary
        // node only reports stepwise/continuous frame sizes (or enumeration
        // fails), `max_resolution` is `None`, not "0x0". It must not be
        // outranked by a sibling IR/secondary node just because that node
        // happens to report a small discrete size.
        let usb_device = PathBuf::from("/sys/devices/usb1/1-1");
        let primary_unknown = camera("Logitech BRIO", None);
        let ir = camera("Logitech BRIO", Some((340, 340)));

        let cameras = merge_by_usb_device([
            (usb_device.clone(), primary_unknown.clone(), true),
            (usb_device, ir, false),
        ]);

        assert_eq!(cameras, vec![primary_unknown]);
    }

    #[test]
    fn color_node_wins_over_an_ir_node_that_enumerates_first() {
        // Reproduces the second #1234 review finding: when both the color
        // and IR node have unknown resolution, `/dev/videoN` enumeration
        // order is not a reliable tiebreaker — an IR node such as
        // `/dev/video10` can sort before its sibling color node
        // `/dev/video2` (nodes() sorts lexicographically by path, and "1" <
        // "2"). The IR node here is first-seen and would win under plain
        // "first-seen wins", but `is_color` must override that.
        let usb_device = PathBuf::from("/sys/devices/usb1/1-1");
        let ir_seen_first = camera("video10-ir", None);
        let color_seen_second = camera("video2-color", None);

        let cameras = merge_by_usb_device([
            (usb_device.clone(), ir_seen_first, false),
            (usb_device, color_seen_second.clone(), true),
        ]);

        assert_eq!(cameras, vec![color_seen_second]);
    }

    #[test]
    fn is_monochrome_fourcc_recognizes_known_ir_formats() {
        for code in [b"GREY", b"Y8  ", b"Y10 ", b"Y12 ", b"Y16 "] {
            assert!(is_monochrome_fourcc(FourCC::new(code)));
        }
        for code in [b"YUYV", b"MJPG", b"NV12"] {
            assert!(!is_monochrome_fourcc(FourCC::new(code)));
        }
    }
}
