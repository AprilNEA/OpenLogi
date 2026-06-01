//! Maintains a registry of well-known HID++2.0 features and their default
//! implementations.

use std::{any::TypeId, collections::HashMap, sync::Arc};

use lazy_static::lazy_static;

use super::Feature;
use crate::{
    channel::HidppChannel,
    feature::{
        CreatableFeature, device_friendly_name::DeviceFriendlyNameFeature,
        device_information::DeviceInformationFeature,
        device_type_and_name::DeviceTypeAndNameFeature, feature_set::FeatureSetFeature,
        hires_wheel::HiResWheelFeature, root::RootFeature, smartshift::SmartShiftFeature,
        thumbwheel::ThumbwheelFeature, unified_battery::UnifiedBatteryFeature,
        wireless_device_status::WirelessDeviceStatusFeature,
    },
};

/// Represents a function that creates a new dynamically sized feature
/// implementation.
pub type FeatureImplProducer =
    fn(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> (TypeId, Arc<dyn Feature>);

/// Represents a known feature implementation starting from a specific feature
/// version.
#[derive(Clone, Copy, Debug, Hash)]
pub struct FeatureVersion {
    /// The minimum feature version the implementation supports.
    pub starting_version: u8,

    /// A pointer to a function producing the feature implementation.
    pub producer: FeatureImplProducer,
}

/// Represents a known HID++2.0 device feature.
#[derive(Clone, Copy, Debug, Hash)]
pub struct KnownFeature {
    /// The name of the feature.
    /// This is usually a slightly modified version of the name found in
    /// Logitech's documentation.
    pub name: &'static str,

    /// A list of concrete implementations of the feature, each supporting the
    /// feature starting from a specific version.
    pub versions: &'static [FeatureVersion],
}

/// Looks up a feature by its ID.
pub fn lookup(feature_id: u16) -> Option<KnownFeature> {
    KNOWN_FEATURES.get(&feature_id).copied()
}

/// Looks up all implementations supporting a specific feature ID and version
/// combination.
pub fn lookup_version(feature_id: u16, feature_version: u8) -> Option<Vec<FeatureVersion>> {
    lookup(feature_id).map(|feat| {
        feat.versions
            .iter()
            .filter(|&ver| ver.starting_version <= feature_version)
            .copied()
            .collect::<Vec<FeatureVersion>>()
    })
}

/// Creates a new feature with a dynamic return type.
fn new_dyn<F: CreatableFeature>(
    chan: Arc<HidppChannel>,
    device_index: u8,
    feature_index: u8,
) -> (TypeId, Arc<dyn Feature>) {
    (
        TypeId::of::<F>(),
        Arc::new(F::new(chan, device_index, feature_index)),
    )
}

lazy_static! {
    static ref KNOWN_FEATURES: HashMap<u16, KnownFeature> = HashMap::from([
        (
            0x0000,
            KnownFeature {
                name: "Root",
                versions: &[FeatureVersion {
                    starting_version: RootFeature::STARTING_VERSION,
                    producer: new_dyn::<RootFeature>
                }]
            }
        ),
        (
            0x0001,
            KnownFeature {
                name: "FeatureSet",
                versions: &[FeatureVersion {
                    starting_version: FeatureSetFeature::STARTING_VERSION,
                    producer: new_dyn::<FeatureSetFeature>
                }]
            }
        ),
        (
            0x0002,
            KnownFeature {
                name: "FeatureInfo",
                versions: &[]
            }
        ),
        (
            0x0003,
            KnownFeature {
                name: "DeviceInformation",
                versions: &[FeatureVersion {
                    starting_version: DeviceInformationFeature::STARTING_VERSION,
                    producer: new_dyn::<DeviceInformationFeature>
                }]
            }
        ),
        (
            0x0004,
            KnownFeature {
                name: "UnitId",
                versions: &[]
            }
        ),
        (
            0x0005,
            KnownFeature {
                name: "DeviceTypeAndName",
                versions: &[FeatureVersion {
                    starting_version: DeviceTypeAndNameFeature::STARTING_VERSION,
                    producer: new_dyn::<DeviceTypeAndNameFeature>
                }]
            }
        ),
        (
            0x0006,
            KnownFeature {
                name: "DeviceGroups",
                versions: &[]
            }
        ),
        (
            0x0007,
            KnownFeature {
                name: "DeviceFriendlyName",
                versions: &[FeatureVersion {
                    starting_version: DeviceFriendlyNameFeature::STARTING_VERSION,
                    producer: new_dyn::<DeviceFriendlyNameFeature>
                }]
            }
        ),
        (
            0x0008,
            KnownFeature {
                name: "KeepAlive",
                versions: &[]
            }
        ),
        (
            0x0020,
            KnownFeature {
                name: "ConfigChange",
                versions: &[]
            }
        ),
        (
            0x0021,
            KnownFeature {
                name: "UniqueRandomId",
                versions: &[]
            }
        ),
        (
            0x0030,
            KnownFeature {
                name: "TargetSoftware",
                versions: &[]
            }
        ),
        (
            0x0080,
            KnownFeature {
                name: "WirelessSignalStrength",
                versions: &[]
            }
        ),
        (
            0x00c0,
            KnownFeature {
                name: "DfuControlLegacy",
                versions: &[]
            }
        ),
        (
            0x00c1,
            KnownFeature {
                name: "DfuControlUnsigned",
                versions: &[]
            }
        ),
        (
            0x00c2,
            KnownFeature {
                name: "DfuControlSigned",
                versions: &[]
            }
        ),
        (
            0x00c3,
            KnownFeature {
                name: "DfuControlBolt",
                versions: &[]
            }
        ),
        (
            0x00d0,
            KnownFeature {
                name: "Dfu",
                versions: &[]
            }
        ),
        (
            0x00d1,
            KnownFeature {
                name: "DfuResumable",
                versions: &[]
            }
        ),
        (
            0x1000,
            KnownFeature {
                name: "BatteryStatus",
                versions: &[]
            }
        ),
        (
            0x1001,
            KnownFeature {
                name: "BatteryVoltage",
                versions: &[]
            }
        ),
        (
            0x1004,
            KnownFeature {
                name: "UnifiedBattery",
                versions: &[FeatureVersion {
                    starting_version: UnifiedBatteryFeature::STARTING_VERSION,
                    producer: new_dyn::<UnifiedBatteryFeature>
                }]
            }
        ),
        (
            0x1010,
            KnownFeature {
                name: "ChargingControl",
                versions: &[]
            }
        ),
        (
            0x1300,
            KnownFeature {
                name: "LedControl",
                versions: &[]
            }
        ),
        (
            0x1800,
            KnownFeature {
                name: "GenericTest",
                versions: &[]
            }
        ),
        (
            0x1802,
            KnownFeature {
                name: "DeviceReset",
                versions: &[]
            }
        ),
        (
            0x1805,
            KnownFeature {
                name: "OobState",
                versions: &[]
            }
        ),
        (
            0x1806,
            KnownFeature {
                name: "ConfigDeviceProps",
                versions: &[]
            }
        ),
        (
            0x1814,
            KnownFeature {
                name: "ChangeHost",
                versions: &[]
            }
        ),
        (
            0x1815,
            KnownFeature {
                name: "HostsInfo",
                versions: &[]
            }
        ),
        (
            0x1981,
            KnownFeature {
                name: "Backlight1",
                versions: &[]
            }
        ),
        (
            0x1982,
            KnownFeature {
                name: "Backlight2",
                versions: &[]
            }
        ),
        (
            0x1983,
            KnownFeature {
                name: "Backlight3",
                versions: &[]
            }
        ),
        (
            0x1990,
            KnownFeature {
                name: "Illumination",
                versions: &[]
            }
        ),
        (
            0x19b0,
            KnownFeature {
                name: "HapticFeedback",
                versions: &[]
            }
        ),
        (
            0x19c0,
            KnownFeature {
                name: "ForceSensingButton",
                versions: &[]
            }
        ),
        (
            0x1a00,
            KnownFeature {
                name: "PresenterControl",
                versions: &[]
            }
        ),
        (
            0x1a01,
            KnownFeature {
                name: "Sensor3D",
                versions: &[]
            }
        ),
        (
            0x1b00,
            KnownFeature {
                name: "ReprogControls",
                versions: &[]
            }
        ),
        (
            0x1b01,
            KnownFeature {
                name: "ReprogControls2",
                versions: &[]
            }
        ),
        (
            0x1b02,
            KnownFeature {
                name: "ReprogControls3",
                versions: &[]
            }
        ),
        (
            0x1b03,
            KnownFeature {
                name: "ReprogControls4",
                versions: &[]
            }
        ),
        (
            0x1b04,
            KnownFeature {
                name: "ReprogControls5",
                versions: &[]
            }
        ),
        (
            0x1bc0,
            KnownFeature {
                name: "ReportHidUsages",
                versions: &[]
            }
        ),
        (
            0x1c00,
            KnownFeature {
                name: "PersistentRemappableAction",
                versions: &[]
            }
        ),
        (
            0x1d4b,
            KnownFeature {
                name: "WirelessDeviceStatus",
                versions: &[FeatureVersion {
                    starting_version: WirelessDeviceStatusFeature::STARTING_VERSION,
                    producer: new_dyn::<WirelessDeviceStatusFeature>
                }]
            }
        ),
        (
            0x1df0,
            KnownFeature {
                name: "RemainingPairings",
                versions: &[]
            }
        ),
        (
            0x1f1f,
            KnownFeature {
                name: "FirmwareProperties",
                versions: &[]
            }
        ),
        (
            0x1f20,
            KnownFeature {
                name: "AdcMeasurement",
                versions: &[]
            }
        ),
        (
            0x2001,
            KnownFeature {
                name: "SwapLeftRightButton",
                versions: &[]
            }
        ),
        (
            0x2005,
            KnownFeature {
                name: "ButtonSwapCancel",
                versions: &[]
            }
        ),
        (
            0x2006,
            KnownFeature {
                name: "PointerAxesOrientation",
                versions: &[]
            }
        ),
        (
            0x2100,
            KnownFeature {
                name: "VerticalScrolling",
                versions: &[]
            }
        ),
        (
            0x2110,
            KnownFeature {
                name: "SmartShiftWheel",
                versions: &[FeatureVersion {
                    starting_version: SmartShiftFeature::STARTING_VERSION,
                    producer: new_dyn::<SmartShiftFeature>
                }]
            }
        ),
        (
            0x2111,
            KnownFeature {
                name: "SmartShiftWheelEnhanced",
                versions: &[]
            }
        ),
        (
            0x2120,
            KnownFeature {
                name: "HighResolutionScrolling",
                versions: &[]
            }
        ),
        (
            0x2121,
            KnownFeature {
                name: "HiResWheel",
                versions: &[FeatureVersion {
                    starting_version: HiResWheelFeature::STARTING_VERSION,
                    producer: new_dyn::<HiResWheelFeature>
                }]
            }
        ),
        (
            0x2130,
            KnownFeature {
                name: "RatchetWheel",
                versions: &[]
            }
        ),
        (
            0x2150,
            KnownFeature {
                name: "Thumbwheel",
                versions: &[FeatureVersion {
                    starting_version: ThumbwheelFeature::STARTING_VERSION,
                    producer: new_dyn::<ThumbwheelFeature>
                }]
            }
        ),
        (
            0x2200,
            KnownFeature {
                name: "MousePointer",
                versions: &[]
            }
        ),
        (
            0x2201,
            KnownFeature {
                name: "AdjustableDpi",
                versions: &[]
            }
        ),
        (
            0x2202,
            KnownFeature {
                name: "ExtendedAdjustableDpi",
                versions: &[]
            }
        ),
        (
            0x2205,
            KnownFeature {
                name: "PointerMotionScaling",
                versions: &[]
            }
        ),
        (
            0x2230,
            KnownFeature {
                name: "SensorAngleSnapping",
                versions: &[]
            }
        ),
        (
            0x2240,
            KnownFeature {
                name: "SurfaceTuning",
                versions: &[]
            }
        ),
        (
            0x2250,
            KnownFeature {
                name: "XyStats",
                versions: &[]
            }
        ),
        (
            0x2251,
            KnownFeature {
                name: "WheelStats",
                versions: &[]
            }
        ),
        (
            0x2400,
            KnownFeature {
                name: "HybridTrackingEngine",
                versions: &[]
            }
        ),
        (
            0x40a0,
            KnownFeature {
                name: "FnInversion",
                versions: &[]
            }
        ),
        (
            0x40a2,
            KnownFeature {
                name: "FnInversionWithDefaultState",
                versions: &[]
            }
        ),
        (
            0x40a3,
            KnownFeature {
                name: "FnInversionForMultiHostDevices",
                versions: &[]
            }
        ),
        (
            0x4100,
            KnownFeature {
                name: "Encryption",
                versions: &[]
            }
        ),
        (
            0x4220,
            KnownFeature {
                name: "LockKeyState",
                versions: &[]
            }
        ),
        (
            0x4301,
            KnownFeature {
                name: "SolarKeyboardDashboard",
                versions: &[]
            }
        ),
        (
            0x4520,
            KnownFeature {
                name: "KeyboardLayout",
                versions: &[]
            }
        ),
        (
            0x4521,
            KnownFeature {
                name: "DisableKeys",
                versions: &[]
            }
        ),
        (
            0x4522,
            KnownFeature {
                name: "DisableKeysByUsage",
                versions: &[]
            }
        ),
        (
            0x4530,
            KnownFeature {
                name: "DualPlatform",
                versions: &[]
            }
        ),
        (
            0x4531,
            KnownFeature {
                name: "MultiPlatform",
                versions: &[]
            }
        ),
        (
            0x4540,
            KnownFeature {
                name: "KeyboardInternationalLayouts",
                versions: &[]
            }
        ),
        (
            0x4600,
            KnownFeature {
                name: "Crown",
                versions: &[]
            }
        ),
        (
            0x6010,
            KnownFeature {
                name: "TouchpadFwItems",
                versions: &[]
            }
        ),
        (
            0x6011,
            KnownFeature {
                name: "TouchpadSwItems",
                versions: &[]
            }
        ),
        (
            0x6012,
            KnownFeature {
                name: "TouchpadWin8FwItems",
                versions: &[]
            }
        ),
        (
            0x6020,
            KnownFeature {
                name: "TapEnable",
                versions: &[]
            }
        ),
        (
            0x6021,
            KnownFeature {
                name: "TapEnableExtended",
                versions: &[]
            }
        ),
        (
            0x6030,
            KnownFeature {
                name: "CursorBallistic",
                versions: &[]
            }
        ),
        (
            0x6040,
            KnownFeature {
                name: "TouchpadResolutionDivider",
                versions: &[]
            }
        ),
        (
            0x6100,
            KnownFeature {
                name: "TouchpadRawXy",
                versions: &[]
            }
        ),
        (
            0x6110,
            KnownFeature {
                name: "TouchMouseRawTouchPoints",
                versions: &[]
            }
        ),
        (
            0x6120,
            KnownFeature {
                name: "BtTouchMouseSettings",
                versions: &[]
            }
        ),
        (
            0x6500,
            KnownFeature {
                name: "Gestures1",
                versions: &[]
            }
        ),
        (
            0x6501,
            KnownFeature {
                name: "Gestures2",
                versions: &[]
            }
        ),
        (
            0x8010,
            KnownFeature {
                name: "GamingGKeys",
                versions: &[]
            }
        ),
        (
            0x8020,
            KnownFeature {
                name: "GamingMKeys",
                versions: &[]
            }
        ),
        (
            0x8030,
            KnownFeature {
                name: "MacroRecord",
                versions: &[]
            }
        ),
        (
            0x8040,
            KnownFeature {
                name: "BrightnessControl",
                versions: &[]
            }
        ),
        (
            0x8060,
            KnownFeature {
                name: "AdjustableReportRate",
                versions: &[]
            }
        ),
        (
            0x8061,
            KnownFeature {
                name: "ExtendedAdjustableReportRate",
                versions: &[]
            }
        ),
        (
            0x8070,
            KnownFeature {
                name: "ColorLedEffects",
                versions: &[]
            }
        ),
        (
            0x8071,
            KnownFeature {
                name: "RgbEffects",
                versions: &[]
            }
        ),
        (
            0x8080,
            KnownFeature {
                name: "PerKeyLighting",
                versions: &[]
            }
        ),
        (
            0x8081,
            KnownFeature {
                name: "PerKeyLighting2",
                versions: &[]
            }
        ),
        (
            0x8090,
            KnownFeature {
                name: "ModeStatus",
                versions: &[]
            }
        ),
        (
            0x8100,
            KnownFeature {
                name: "OnboardProfiles",
                versions: &[]
            }
        ),
        (
            0x8110,
            KnownFeature {
                name: "MouseButtonFilter",
                versions: &[]
            }
        ),
        (
            0x8111,
            KnownFeature {
                name: "LatencyMonitoring",
                versions: &[]
            }
        ),
        (
            0x8120,
            KnownFeature {
                name: "GamingAttachments",
                versions: &[]
            }
        ),
        (
            0x8123,
            KnownFeature {
                name: "ForceFeedback",
                versions: &[]
            }
        ),
        (
            0x8300,
            KnownFeature {
                name: "Sidetone",
                versions: &[]
            }
        ),
        (
            0x8310,
            KnownFeature {
                name: "Equalizer",
                versions: &[]
            }
        ),
        (
            0x8320,
            KnownFeature {
                name: "HeadsetOut",
                versions: &[]
            }
        ),
    ]);
}
