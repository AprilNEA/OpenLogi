//! Built-in implementation identities. Protocol ownership stays with each driver.

/// A compiled driver binding, independent of product and transport identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinDriver {
    /// Existing HID++ receiver and direct-device sessions.
    Hidpp,
    /// Existing Litra raw-HID sessions.
    Litra,
    /// Native UVC controls and media capture.
    Camera,
    /// Host device-scoped HID usage mapping.
    NativeRemap,
}

impl BuiltinDriver {
    /// Explicit compiled binding table.
    pub const ALL: [Self; 4] = [Self::Hidpp, Self::Litra, Self::Camera, Self::NativeRemap];

    /// Stable implementation identifier used by descriptors and configuration.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Hidpp => crate::HIDPP_DRIVER_ID,
            Self::Litra => crate::litra::LITRA_DRIVER_ID,
            Self::Camera => "org.openlogi.uvc",
            Self::NativeRemap => crate::native_remap::NATIVE_REMAP_DRIVER_ID,
        }
    }

    /// Resolve a requested built-in implementation without probing hardware.
    #[must_use]
    pub fn find(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|driver| driver.id() == id)
    }

    /// Classify protocol collections without opening a device.
    /// Host discovery must additionally exclude virtual receiver child nodes.
    #[must_use]
    pub fn for_hid(vendor: u16, product: u16, page: u16, usage: u16) -> Option<Self> {
        if crate::litra::matches_litra(vendor, product, page, usage) {
            Some(Self::Litra)
        } else if vendor == crate::LOGITECH_VENDOR_ID
            && HidppReports::for_collection(page, usage).is_some()
        {
            Some(Self::Hidpp)
        } else {
            None
        }
    }
}

/// HID++ framing established by the collection's HID identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HidppReports {
    /// Receiver, USB, and Bluetooth classic collections carry both widths.
    ShortAndLong,
    /// Bluetooth LE collections require short requests to use long reports.
    LongOnly,
}

impl HidppReports {
    /// Recognize the one long-report collection used by each supported protocol transport.
    #[must_use]
    pub const fn for_collection(page: u16, usage: u16) -> Option<Self> {
        match (page, usage) {
            (0xff00, 0x0002) | (0xff43, 0x0602) => Some(Self::ShortAndLong),
            (0xff43, 0x0202) => Some(Self::LongOnly),
            _ => None,
        }
    }
}
