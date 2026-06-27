//! Maintains a registry of well-known HID++2.0 features and their default
//! implementations.

use std::{
    any::TypeId,
    collections::HashMap,
    sync::{Arc, LazyLock},
};

use super::Feature;
use crate::{
    channel::HidppChannel,
    feature::{
        CreatableFeature,
        adjustable_dpi::AdjustableDpiFeature,
        backlight::BacklightFeature,
        brightness_control::BrightnessControlFeature,
        change_host::ChangeHostFeature,
        color_led_effects::ColorLedEffectsFeature,
        crown::CrownFeature,
        device_friendly_name::DeviceFriendlyNameFeature,
        device_information::DeviceInformationFeature,
        device_type_and_name::DeviceTypeAndNameFeature,
        disable_keys::DisableKeysFeature,
        disable_keys_by_usage::DisableKeysByUsageFeature,
        dual_platform::DualPlatformFeature,
        equalizer::EqualizerFeature,
        extended_dpi::ExtendedDpiFeature,
        extended_report_rate::ExtendedReportRateFeature,
        feature_set::FeatureSetFeature,
        fn_inversion::{FnInversionMultiHostFeature, FnInversionWithDefaultStateFeature},
        hires_wheel::HiResWheelFeature,
        hosts_info::HostsInfoFeature,
        illumination::IlluminationFeature,
        mode_status::ModeStatusFeature,
        mouse_pointer::MousePointerFeature,
        multi_platform::MultiPlatformFeature,
        per_key_lighting::PerKeyLightingFeature,
        persistent_remappable_action::PersistentRemappableActionFeature,
        report_rate::ReportRateFeature,
        reprog_controls::ReprogControlsFeature,
        rgb_effects::RgbEffectsFeature,
        root::RootFeature,
        sidetone::SidetoneFeature,
        smartshift::SmartShiftFeature,
        smartshift_enhanced::SmartShiftEnhancedFeature,
        thumbwheel::ThumbwheelFeature,
        unified_battery::UnifiedBatteryFeature,
        vertical_scrolling::VerticalScrollingFeature,
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

/// Builds [`KNOWN_FEATURES`]. Each row is `id "Name"` for a feature we only know
/// by name, or `id "Name" => Impl, ...` to also register one or more default
/// implementations through [`new_dyn`]. Listing several impls mirrors a feature
/// that ships multiple versions, each contributing its own
/// [`CreatableFeature::STARTING_VERSION`] in declaration order.
macro_rules! known_features {
    ( $( $id:literal $name:literal $( => $($feat:ty),+ )? ),* $(,)? ) => {
        HashMap::from([ $(
            ($id, KnownFeature { name: $name, versions: known_features!(@versions $( $($feat),+ )?) }),
        )* ])
    };
    (@versions) => { &[] };
    (@versions $($feat:ty),+) => {
        &[$(FeatureVersion {
            starting_version: <$feat>::STARTING_VERSION,
            producer: new_dyn::<$feat>,
        }),+]
    };
}

static KNOWN_FEATURES: LazyLock<HashMap<u16, KnownFeature>> = LazyLock::new(|| {
    known_features! {
    0x0000 "Root" => RootFeature,
    0x0001 "FeatureSet" => FeatureSetFeature,
    0x0002 "FeatureInfo",
    0x0003 "DeviceInformation" => DeviceInformationFeature,
    0x0004 "UnitId",
    0x0005 "DeviceTypeAndName" => DeviceTypeAndNameFeature,
    0x0006 "DeviceGroups",
    0x0007 "DeviceFriendlyName" => DeviceFriendlyNameFeature,
    0x0008 "KeepAlive",
    0x0020 "ConfigChange",
    0x0021 "UniqueRandomId",
    0x0030 "TargetSoftware",
    0x0080 "WirelessSignalStrength",
    0x00c0 "DfuControlLegacy",
    0x00c1 "DfuControlUnsigned",
    0x00c2 "DfuControlSigned",
    0x00c3 "DfuControlBolt",
    0x00d0 "Dfu",
    0x00d1 "DfuResumable",
    0x1000 "BatteryStatus",
    0x1001 "BatteryVoltage",
    0x1004 "UnifiedBattery" => UnifiedBatteryFeature,
    0x1010 "ChargingControl",
    0x1300 "LedControl",
    0x1800 "GenericTest",
    0x1802 "DeviceReset",
    0x1805 "OobState",
    0x1806 "ConfigDeviceProps",
    0x1814 "ChangeHost" => ChangeHostFeature,
    0x1815 "HostsInfo" => HostsInfoFeature,
    0x1981 "Backlight1",
    0x1982 "Backlight2" => BacklightFeature,
    0x1983 "Backlight3",
    0x1990 "Illumination" => IlluminationFeature,
    0x19b0 "HapticFeedback",
    0x19c0 "ForceSensingButton",
    0x1a00 "PresenterControl",
    0x1a01 "Sensor3D",
    0x1b00 "ReprogControls",
    0x1b01 "ReprogControls2",
    0x1b02 "ReprogControls3",
    0x1b03 "ReprogControls4",
    0x1b04 "ReprogControls5" => ReprogControlsFeature,
    0x1bc0 "ReportHidUsages",
    0x1c00 "PersistentRemappableAction" => PersistentRemappableActionFeature,
    0x1d4b "WirelessDeviceStatus" => WirelessDeviceStatusFeature,
    0x1df0 "RemainingPairings",
    0x1f1f "FirmwareProperties",
    0x1f20 "AdcMeasurement",
    0x2001 "SwapLeftRightButton",
    0x2005 "ButtonSwapCancel",
    0x2006 "PointerAxesOrientation",
    0x2100 "VerticalScrolling" => VerticalScrollingFeature,
    0x2110 "SmartShiftWheel" => SmartShiftFeature,
    0x2111 "SmartShiftWheelEnhanced" => SmartShiftEnhancedFeature,
    0x2120 "HighResolutionScrolling",
    0x2121 "HiResWheel" => HiResWheelFeature,
    0x2130 "RatchetWheel",
    0x2150 "Thumbwheel" => ThumbwheelFeature,
    0x2200 "MousePointer" => MousePointerFeature,
    0x2201 "AdjustableDpi" => AdjustableDpiFeature,
    0x2202 "ExtendedAdjustableDpi" => ExtendedDpiFeature,
    0x2205 "PointerMotionScaling",
    0x2230 "SensorAngleSnapping",
    0x2240 "SurfaceTuning",
    0x2250 "XyStats",
    0x2251 "WheelStats",
    0x2400 "HybridTrackingEngine",
    0x40a0 "FnInversion",
    0x40a2 "FnInversionWithDefaultState" => FnInversionWithDefaultStateFeature,
    0x40a3 "FnInversionForMultiHostDevices" => FnInversionMultiHostFeature,
    0x4100 "Encryption",
    0x4220 "LockKeyState",
    0x4301 "SolarKeyboardDashboard",
    0x4520 "KeyboardLayout",
    0x4521 "DisableKeys" => DisableKeysFeature,
    0x4522 "DisableKeysByUsage" => DisableKeysByUsageFeature,
    0x4530 "DualPlatform" => DualPlatformFeature,
    0x4531 "MultiPlatform" => MultiPlatformFeature,
    0x4540 "KeyboardInternationalLayouts",
    0x4600 "Crown" => CrownFeature,
    0x6010 "TouchpadFwItems",
    0x6011 "TouchpadSwItems",
    0x6012 "TouchpadWin8FwItems",
    0x6020 "TapEnable",
    0x6021 "TapEnableExtended",
    0x6030 "CursorBallistic",
    0x6040 "TouchpadResolutionDivider",
    0x6100 "TouchpadRawXy",
    0x6110 "TouchMouseRawTouchPoints",
    0x6120 "BtTouchMouseSettings",
    0x6500 "Gestures1",
    0x6501 "Gestures2",
    0x8010 "GamingGKeys",
    0x8020 "GamingMKeys",
    0x8030 "MacroRecord",
    0x8040 "BrightnessControl" => BrightnessControlFeature,
    0x8060 "AdjustableReportRate" => ReportRateFeature,
    0x8061 "ExtendedAdjustableReportRate" => ExtendedReportRateFeature,
    0x8070 "ColorLedEffects" => ColorLedEffectsFeature,
    0x8071 "RgbEffects" => RgbEffectsFeature,
    0x8080 "PerKeyLighting",
    0x8081 "PerKeyLighting2" => PerKeyLightingFeature,
    0x8090 "ModeStatus" => ModeStatusFeature,
    0x8100 "OnboardProfiles",
    0x8110 "MouseButtonFilter",
    0x8111 "LatencyMonitoring",
    0x8120 "GamingAttachments",
    0x8123 "ForceFeedback",
    0x8300 "Sidetone" => SidetoneFeature,
    0x8310 "Equalizer" => EqualizerFeature,
    0x8320 "HeadsetOut",
    }
});

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{FeatureVersion, KnownFeature, new_dyn};
    use crate::feature::{CreatableFeature, feature_set::FeatureSetFeature, root::RootFeature};

    #[test]
    fn macro_registers_one_version_per_listed_impl() {
        // The `=> A, B` form keeps the original table's ability to register
        // several versioned implementations under a single feature id.
        let map: HashMap<u16, KnownFeature> = known_features! {
            0x0000 "NameOnly",
            0x0001 "OneImpl" => RootFeature,
            0xffff "TwoImpls" => RootFeature, FeatureSetFeature,
        };

        assert_eq!(map[&0x0000].versions.len(), 0);
        assert_eq!(map[&0x0001].versions.len(), 1);
        assert_eq!(map[&0xffff].versions.len(), 2);
    }
}
