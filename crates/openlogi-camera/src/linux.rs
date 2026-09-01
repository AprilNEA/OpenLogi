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
    let described = nodes()
        .into_iter()
        .map(|node| (node.usb_device.clone(), describe(&node)));
    merge_by_usb_device(described)
}

/// Collapse `(usb_device, Camera)` pairs to one `Camera` per distinct
/// `usb_device`, keeping the highest-resolution entry in each group and
/// otherwise preserving first-seen order.
fn merge_by_usb_device(nodes: impl IntoIterator<Item = (PathBuf, Camera)>) -> Vec<Camera> {
    let mut by_device: Vec<(PathBuf, Camera)> = Vec::new();
    for (usb_device, camera) in nodes {
        match by_device.iter_mut().find(|(dev, _)| *dev == usb_device) {
            Some((_, best)) => {
                if resolution_area(camera.max_resolution) > resolution_area(best.max_resolution) {
                    *best = camera;
                }
            }
            None => by_device.push((usb_device, camera)),
        }
    }
    by_device.into_iter().map(|(_, camera)| camera).collect()
}

/// Pixel count of a resolution, for comparing which capture node is the
/// primary sensor. `None` sorts as smallest.
fn resolution_area(resolution: Option<(u32, u32)>) -> u64 {
    resolution.map_or(0, |(w, h)| u64::from(w) * u64::from(h))
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

        let cameras = merge_by_usb_device([(usb_device.clone(), main.clone()), (usb_device, ir)]);

        assert_eq!(cameras, vec![main]);
    }

    #[test]
    fn distinct_usb_devices_stay_separate() {
        let one = camera("Logitech BRIO", Some((4096, 2160)));
        let two = camera("Logitech StreamCam", Some((1920, 1080)));

        let cameras = merge_by_usb_device([
            (PathBuf::from("/sys/devices/usb1/1-1"), one.clone()),
            (PathBuf::from("/sys/devices/usb1/1-2"), two.clone()),
        ]);

        assert_eq!(cameras, vec![one, two]);
    }

    #[test]
    fn resolution_area_treats_unknown_resolution_as_smallest() {
        assert_eq!(resolution_area(None), 0);
        assert_eq!(resolution_area(Some((340, 340))), 340 * 340);
        assert!(resolution_area(Some((4096, 2160))) > resolution_area(Some((340, 340))));
    }
}
