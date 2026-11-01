//! The VideoControl topology a UVC camera declares in its configuration
//! descriptor: which interface takes the class requests, and the entity ids of
//! its Processing Unit, camera terminal and Extension Units.
//!
//! macOS reads the descriptor through IOKit and Linux from the USB device's
//! sysfs `descriptors` file; both hand it here as plain bytes. Windows needs
//! none of it, because kernel streaming already exposes each unit as a
//! topology node.

use crate::extension_unit::{ExtensionControl, ExtensionGuid};

const CC_VIDEO: u8 = 0x0E;
const SC_VIDEOCONTROL: u8 = 0x01;
const DESC_INTERFACE: u8 = 0x04;
const DESC_CS_INTERFACE: u8 = 0x24;
const VC_INPUT_TERMINAL: u8 = 0x02;
const VC_PROCESSING_UNIT: u8 = 0x05;
const VC_EXTENSION_UNIT: u8 = 0x06;
/// wTerminalType for a camera sensor input terminal (ITT_CAMERA).
pub(crate) const ITT_CAMERA: u16 = 0x0201;

/// Offset of `guidExtensionCode` in an Extension Unit descriptor; `bUnitID`
/// precedes it at offset 3, and `bNumControls` and `bNrInPins` follow it.
const XU_GUID: usize = 4;
/// Offset of `bNrInPins`: that many one-byte source ids sit before
/// `bControlSize` and the `bmControls` bitmap.
const XU_INPUT_PINS: usize = 21;

/// The VideoControl entities a control request can address.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct VcTopology {
    pub(crate) vc_interface: u8,
    pub(crate) processing_unit: u8,
    /// `None` when the descriptor lists no camera terminal: lens controls then
    /// report `Unsupported`.
    pub(crate) camera_terminal: Option<u8>,
    extension_units: Vec<ExtensionUnit>,
}

impl VcTopology {
    /// The entity id of the Extension Unit that carries `control`, when the
    /// camera declares that unit and its bitmap lists the selector.
    pub(crate) fn extension_unit(&self, control: ExtensionControl) -> Option<u8> {
        self.extension_units
            .iter()
            .find(|unit| unit.guid == control.guid && unit.implements(control.selector))
            .map(|unit| unit.unit_id)
    }
}

/// One Extension Unit: the GUID naming its control set, the entity id
/// requests address, and the `bmControls` bitmap of selectors it implements.
#[derive(Debug, PartialEq, Eq)]
struct ExtensionUnit {
    guid: ExtensionGuid,
    unit_id: u8,
    /// Bit `n` set means selector `n + 1` is implemented.
    controls: Vec<u8>,
}

impl ExtensionUnit {
    /// Whether the bitmap lists `selector`. Selector 0 is reserved and never
    /// implemented; a selector past the bitmap's end is not implemented.
    fn implements(&self, selector: u8) -> bool {
        let Some(bit) = selector.checked_sub(1) else {
            return false;
        };
        self.controls
            .get(usize::from(bit / 8))
            .is_some_and(|byte| byte & (1 << (bit % 8)) != 0)
    }

    /// Parse an Extension Unit descriptor, or `None` when it is too short for
    /// the lengths it declares.
    fn parse(descriptor: &[u8]) -> Option<Self> {
        let unit_id = *descriptor.get(3)?;
        let guid: [u8; 16] = descriptor.get(XU_GUID..XU_GUID + 16)?.try_into().ok()?;
        let pins = usize::from(*descriptor.get(XU_INPUT_PINS)?);
        let size_at = XU_INPUT_PINS + 1 + pins;
        let size = usize::from(*descriptor.get(size_at)?);
        let controls = descriptor.get(size_at + 1..size_at + 1 + size)?.to_vec();
        Some(Self {
            guid: ExtensionGuid::from_descriptor(guid),
            unit_id,
            controls,
        })
    }
}

/// The VideoControl interface the descriptor walk is currently inside, and the
/// entities seen in it so far.
///
/// A class-specific descriptor belongs to the interface it follows, so this is
/// dropped on leaving the block: VideoStreaming reuses descriptor type `0x24`
/// with its own subtype numbering, in which `0x05` is a frame descriptor rather
/// than a Processing Unit and `0x02` an output header rather than an input
/// terminal. Tracking the block — instead of a bare "have we seen a
/// VideoControl interface" flag — is what keeps a frame index from being read
/// as a Processing-Unit id on a camera whose VideoControl block has none.
struct VcBlock {
    interface: u8,
    processing_unit: Option<u8>,
    camera_terminal: Option<u8>,
    extension_units: Vec<ExtensionUnit>,
}

impl VcBlock {
    fn new(interface: u8) -> Self {
        Self {
            interface,
            processing_unit: None,
            camera_terminal: None,
            extension_units: Vec::new(),
        }
    }

    /// Record one class-specific descriptor of this block.
    fn record(&mut self, descriptor: &[u8]) {
        let (Some(&subtype), Some(&entity)) = (descriptor.get(2), descriptor.get(3)) else {
            return;
        };
        match subtype {
            // bTerminalID sits at offset 3 like bUnitID; an input terminal's
            // wTerminalType (offsets 4..6) must be the camera sensor — skip
            // composite/other input terminals.
            VC_INPUT_TERMINAL if descriptor.len() >= 8 => {
                let terminal_type = u16::from_le_bytes([descriptor[4], descriptor[5]]);
                if terminal_type == ITT_CAMERA && self.camera_terminal.is_none() {
                    self.camera_terminal = Some(entity);
                }
            }
            VC_PROCESSING_UNIT if self.processing_unit.is_none() => {
                self.processing_unit = Some(entity);
            }
            VC_EXTENSION_UNIT => {
                if let Some(unit) = ExtensionUnit::parse(descriptor) {
                    self.extension_units.push(unit);
                }
            }
            _ => {}
        }
    }

    /// The finished topology, if the block declared a Processing Unit.
    fn finish(self) -> Option<VcTopology> {
        Some(VcTopology {
            vc_interface: self.interface,
            processing_unit: self.processing_unit?,
            camera_terminal: self.camera_terminal,
            extension_units: self.extension_units,
        })
    }
}

/// Walk a descriptor blob, returning the first VideoControl interface that
/// declares a Processing Unit, with every entity that block lists.
///
/// The blob may hold more than one configuration (sysfs concatenates them
/// after the device descriptor); descriptors outside a VideoControl block are
/// skipped. The walk stops at the first descriptor whose `bLength` is nonsense
/// or would run past the blob, so a malformed descriptor truncates the scan
/// rather than misreading the bytes after it.
pub(crate) fn scan_descriptors(blob: &[u8]) -> Option<VcTopology> {
    let mut rest = blob;
    let mut block: Option<VcBlock> = None;
    while rest.len() >= 2 {
        let len = usize::from(rest[0]);
        let dtype = rest[1];
        if len < 2 || len > rest.len() {
            break;
        }
        let (descriptor, tail) = rest.split_at(len);
        rest = tail;

        if dtype == DESC_INTERFACE {
            if let Some(topology) = block.take().and_then(VcBlock::finish) {
                return Some(topology);
            }
            // bInterfaceNumber, bInterfaceClass and bInterfaceSubClass sit at
            // offsets 2, 5 and 6 of an interface descriptor.
            block = match (descriptor.get(2), descriptor.get(5), descriptor.get(6)) {
                (Some(&interface), Some(&CC_VIDEO), Some(&SC_VIDEOCONTROL)) => {
                    Some(VcBlock::new(interface))
                }
                _ => None,
            };
        } else if dtype == DESC_CS_INTERFACE
            && let Some(block) = block.as_mut()
        {
            block.record(descriptor);
        }
    }
    block.and_then(VcBlock::finish)
}

#[cfg(test)]
mod tests {
    use super::{ITT_CAMERA, VcTopology, scan_descriptors};
    use crate::controls::CameraControl;
    use crate::extension_unit::ExtensionControl;

    /// A 9-byte interface descriptor with the given number/class/subclass.
    fn interface(number: u8, class: u8, subclass: u8) -> Vec<u8> {
        vec![9, 0x04, number, 0, 0, class, subclass, 0, 0]
    }

    /// An 8-byte VC_INPUT_TERMINAL descriptor for `entity`.
    fn input_terminal(entity: u8, terminal_type: u16) -> Vec<u8> {
        let [type_lo, type_hi] = terminal_type.to_le_bytes();
        vec![8, 0x24, 0x02, entity, type_lo, type_hi, 0, 0]
    }

    /// A minimal VC_PROCESSING_UNIT descriptor for `entity`.
    fn processing_unit(entity: u8) -> Vec<u8> {
        vec![4, 0x24, 0x05, entity]
    }

    /// The Brio 500's video-pipe GUID as its descriptor stores it.
    const VIDEO_PIPE_BYTES: [u8; 16] = [
        0x15, 0x02, 0xe4, 0x49, 0x34, 0xf4, 0xfe, 0x47, 0xb1, 0x58, 0x0e, 0x88, 0x50, 0x23, 0xe5,
        0x1b,
    ];

    /// A VC_EXTENSION_UNIT descriptor with one input pin.
    fn extension_unit(entity: u8, guid: [u8; 16], bm_controls: &[u8]) -> Vec<u8> {
        let mut descriptor = vec![0, 0x24, 0x06, entity];
        descriptor.extend_from_slice(&guid);
        descriptor.push(13); // bNumControls — the bitmap, not this, is the authority
        descriptor.push(1); // bNrInPins
        descriptor.push(2); // the one source id
        descriptor.push(u8::try_from(bm_controls.len()).expect("short bitmap"));
        descriptor.extend_from_slice(bm_controls);
        descriptor.push(0); // iExtension
        descriptor[0] = u8::try_from(descriptor.len()).expect("short descriptor");
        descriptor
    }

    fn field_of_view() -> ExtensionControl {
        CameraControl::FieldOfView
            .extension()
            .expect("vendor control")
    }

    fn show_mode() -> ExtensionControl {
        CameraControl::ShowMode.extension().expect("vendor control")
    }

    #[test]
    fn finds_the_processing_unit_behind_a_videocontrol_interface() {
        let blob: Vec<u8> = [
            vec![9, 0x02, 0, 0, 0, 0, 0, 0, 0], // configuration header
            interface(3, 0x0E, 0x01),           // VideoControl
            input_terminal(1, ITT_CAMERA),
            processing_unit(2),
        ]
        .concat();
        let topology = scan_descriptors(&blob).expect("a VideoControl block");
        assert_eq!(
            (
                topology.vc_interface,
                topology.processing_unit,
                topology.camera_terminal
            ),
            (3, 2, Some(1))
        );
        assert_eq!(topology.extension_unit(field_of_view()), None);
    }

    /// The Brio 500's video-pipe unit: id 10, bitmap `0xBA 0x1F` — selectors 2, 4,
    /// 5, 6, 8, 9, 10, 11, 12 and 13 (#1222). It follows the Processing Unit,
    /// so the walk must not stop at the first unit it can use.
    #[test]
    fn collects_extension_units_declared_after_the_processing_unit() {
        let blob: Vec<u8> = [
            interface(0, 0x0E, 0x01),
            input_terminal(1, ITT_CAMERA),
            processing_unit(3),
            extension_unit(10, VIDEO_PIPE_BYTES, &[0xBA, 0x1F]),
            interface(1, 0x0E, 0x02), // VideoStreaming
        ]
        .concat();
        let topology = scan_descriptors(&blob).expect("a VideoControl block");
        assert_eq!(topology.processing_unit, 3);
        assert_eq!(topology.extension_unit(field_of_view()), Some(10));
        assert_eq!(topology.extension_unit(show_mode()), Some(10));
    }

    /// The bitmap is the authority: a unit with the right GUID that does not
    /// list the selector does not carry the control.
    #[test]
    fn a_selector_missing_from_the_bitmap_is_not_offered() {
        // Bit 4 (selector 5) cleared; bit 12 (selector 13) still set.
        let blob: Vec<u8> = [
            interface(0, 0x0E, 0x01),
            extension_unit(10, VIDEO_PIPE_BYTES, &[0xAA, 0x1F]),
            processing_unit(3),
        ]
        .concat();
        let topology = scan_descriptors(&blob).expect("a VideoControl block");
        assert_eq!(topology.extension_unit(field_of_view()), None);
        assert_eq!(topology.extension_unit(show_mode()), Some(10));
    }

    /// A one-byte bitmap ends at selector 8, so selector 13 is past its end.
    #[test]
    fn a_selector_past_the_bitmap_is_not_offered() {
        let blob: Vec<u8> = [
            interface(0, 0x0E, 0x01),
            processing_unit(3),
            extension_unit(10, VIDEO_PIPE_BYTES, &[0xFF]),
        ]
        .concat();
        let topology = scan_descriptors(&blob).expect("a VideoControl block");
        assert_eq!(topology.extension_unit(field_of_view()), Some(10));
        assert_eq!(topology.extension_unit(show_mode()), None);
    }

    #[test]
    fn another_vendors_unit_is_not_the_video_pipe() {
        let mut other = VIDEO_PIPE_BYTES;
        other[0] ^= 0xFF;
        let blob: Vec<u8> = [
            interface(0, 0x0E, 0x01),
            processing_unit(3),
            extension_unit(10, other, &[0xFF, 0xFF]),
        ]
        .concat();
        let topology = scan_descriptors(&blob).expect("a VideoControl block");
        assert_eq!(topology.extension_unit(field_of_view()), None);
    }

    /// An Extension Unit whose declared pin count or bitmap size runs past its
    /// own bLength is dropped, not read out of the next descriptor.
    #[test]
    fn a_truncated_extension_unit_is_dropped() {
        let mut truncated = extension_unit(10, VIDEO_PIPE_BYTES, &[0xBA, 0x1F]);
        truncated[22 + 1] = 9; // bControlSize claims nine bitmap bytes
        let blob: Vec<u8> = [interface(0, 0x0E, 0x01), processing_unit(3), truncated].concat();
        let topology = scan_descriptors(&blob).expect("a VideoControl block");
        assert_eq!(topology.extension_unit(field_of_view()), None);
    }

    /// sysfs prefixes the configurations with the 18-byte device descriptor.
    #[test]
    fn a_leading_device_descriptor_is_skipped() {
        let mut device = vec![0u8; 18];
        device[0] = 18;
        device[1] = 0x01;
        let blob: Vec<u8> = [
            device,
            vec![9, 0x02, 0, 0, 0, 0, 0, 0, 0],
            interface(0, 0x0E, 0x01),
            processing_unit(4),
        ]
        .concat();
        assert_eq!(
            scan_descriptors(&blob).map(|topology| topology.processing_unit),
            Some(4)
        );
    }

    /// Composite/other input terminals are not the camera sensor, so lens
    /// controls must report unsupported rather than address the wrong entity.
    #[test]
    fn a_non_camera_input_terminal_leaves_lens_controls_unsupported() {
        let blob: Vec<u8> = [
            interface(0, 0x0E, 0x01),
            input_terminal(1, 0x0401), // ITT_MEDIA_TRANSPORT_INPUT
            processing_unit(5),
        ]
        .concat();
        assert_eq!(
            scan_descriptors(&blob).map(|topology| topology.camera_terminal),
            Some(None)
        );
    }

    /// Class-specific descriptors before any VideoControl interface belong to
    /// some other function and must not be read as UVC entities.
    #[test]
    fn class_descriptors_outside_a_videocontrol_interface_are_ignored() {
        let blob: Vec<u8> = [
            interface(0, 0x01, 0x01), // audio
            processing_unit(9),
        ]
        .concat();
        assert_eq!(scan_descriptors(&blob), None::<VcTopology>);
    }

    /// …and neither do the ones *after* it. VideoStreaming reuses descriptor
    /// type 0x24 with its own subtype numbering, where 0x05 is
    /// VS_FRAME_UNCOMPRESSED rather than VC_PROCESSING_UNIT. A camera whose
    /// VideoControl block has no Processing Unit must report none, not the
    /// first frame descriptor's bFrameIndex — which would send every image
    /// control to whatever entity happens to share that id.
    #[test]
    fn a_videostreaming_frame_descriptor_is_not_a_processing_unit() {
        // A real VS_FRAME_UNCOMPRESSED: 30 bytes, descriptor type 0x24 like a
        // VideoControl unit, subtype 0x05, and bFrameIndex sitting exactly
        // where a unit keeps its bUnitID.
        let mut vs_frame = vec![0u8; 30];
        vs_frame[0] = 30;
        vs_frame[1] = 0x24;
        vs_frame[2] = 0x05;
        vs_frame[3] = 1;
        let blob: Vec<u8> = [
            interface(0, 0x0E, 0x01), // VideoControl — no processing unit
            input_terminal(1, ITT_CAMERA),
            interface(1, 0x0E, 0x02), // VideoStreaming
            vs_frame,
        ]
        .concat();
        assert_eq!(scan_descriptors(&blob), None);
    }

    /// A descriptor whose bLength overruns the blob truncates the walk instead
    /// of reading past it — and a zero length must not loop forever.
    #[test]
    fn malformed_lengths_stop_the_walk() {
        let overrun: Vec<u8> = [interface(0, 0x0E, 0x01), vec![64, 0x24, 0x05, 7]].concat();
        assert_eq!(scan_descriptors(&overrun), None);

        let zero_length: Vec<u8> = [interface(0, 0x0E, 0x01), vec![0, 0x24]].concat();
        assert_eq!(scan_descriptors(&zero_length), None);
    }
}
