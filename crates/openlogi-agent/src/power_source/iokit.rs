//! Dynamically loaded private IOKit power-source API. Nothing is loaded until opt-in.

#![expect(
    unsafe_code,
    reason = "Private IOPS symbols have no generated bindings"
)]

use std::collections::BTreeMap;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr::{self, NonNull};
use std::sync::OnceLock;

use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use tracing::warn;

use super::{AccessoryPower, BatteryStatus, PowerSourceBackend, PowerSourceError};

const IOKIT: &CStr = c"/System/Library/Frameworks/IOKit.framework/IOKit";
const RTLD_LAZY: c_int = 0x1;

type CreateFn = unsafe extern "C" fn(*mut *mut c_void) -> i32;
type SetDetailsFn = unsafe extern "C" fn(*mut c_void, &CFDictionary<CFString, CFType>) -> i32;
type ReleaseFn = unsafe extern "C" fn(*mut c_void) -> i32;

struct Spi {
    create: CreateFn,
    set_details: SetDetailsFn,
    release: ReleaseFn,
}

fn spi() -> Result<&'static Spi, PowerSourceError> {
    static SPI: OnceLock<Result<Spi, PowerSourceError>> = OnceLock::new();
    SPI.get_or_init(load_spi).as_ref().map_err(Clone::clone)
}

fn load_spi() -> Result<Spi, PowerSourceError> {
    // SAFETY: NUL-terminated absolute framework path; the image stays loaded for
    // the process lifetime so its function pointers cannot be invalidated.
    let handle = unsafe { dlopen(IOKIT.as_ptr(), RTLD_LAZY) };
    if handle.is_null() {
        return Err(PowerSourceError::Unavailable(
            "IOKit.framework could not be loaded",
        ));
    }
    let symbol = |name: &CStr| {
        // SAFETY: The framework handle and NUL-terminated symbol name are valid.
        let address = unsafe { dlsym(handle, name.as_ptr()) };
        NonNull::new(address).ok_or(PowerSourceError::Unavailable(
            "This macOS version does not export the required IOPS power-source functions",
        ))
    };
    let create = symbol(c"IOPSCreatePowerSource")?;
    let set_details = symbol(c"IOPSSetPowerSourceDetails")?;
    let release = symbol(c"IOPSReleasePowerSource")?;
    Ok(Spi {
        // SAFETY: Signatures match Apple's IOPowerSourcesPrivate.h. Each symbol
        // is non-null and its framework stays loaded for the process lifetime.
        create: unsafe { std::mem::transmute::<*mut c_void, CreateFn>(create.as_ptr()) },
        // SAFETY: The CFDictionary reference is ABI-compatible with CFDictionaryRef.
        set_details: unsafe {
            std::mem::transmute::<*mut c_void, SetDetailsFn>(set_details.as_ptr())
        },
        // SAFETY: This non-null symbol has the ReleaseFn signature in Apple's header.
        release: unsafe { std::mem::transmute::<*mut c_void, ReleaseFn>(release.as_ptr()) },
    })
}

fn check(operation: &'static str, code: i32) -> Result<(), PowerSourceError> {
    if code == 0 {
        Ok(())
    } else {
        Err(PowerSourceError::Operation { operation, code })
    }
}

/// Owns one opaque, non-CF handle. Native release consumes it even on failure.
struct Source {
    handle: Option<NonNull<c_void>>,
    spi: &'static Spi,
}

// SAFETY: IOPS serializes all create/set/release operations on its own dispatch
// queue (IOPowerSourcesPrivate.c). This unique owner never shares a mutable handle.
unsafe impl Send for Source {}

impl Source {
    fn create(spi: &'static Spi) -> Result<Self, PowerSourceError> {
        let mut handle = ptr::null_mut();
        // SAFETY: The out-pointer is writable; IOPS returns a newly owned handle.
        check("IOPSCreatePowerSource", unsafe {
            (spi.create)(&raw mut handle)
        })?;
        Ok(Self {
            handle: Some(NonNull::new(handle).ok_or(PowerSourceError::MissingHandle)?),
            spi,
        })
    }

    fn set(&self, accessory: &AccessoryPower) -> Result<(), PowerSourceError> {
        let handle = self.handle.ok_or(PowerSourceError::MissingHandle)?;
        let details = details_dictionary(accessory);
        // SAFETY: The live handle is uniquely owned; the dictionary remains valid
        // during the call and IOPS copies its contents for later resynchronization.
        check("IOPSSetPowerSourceDetails", unsafe {
            (self.spi.set_details)(handle.as_ptr(), &details)
        })
    }

    fn release(&mut self) -> Result<(), PowerSourceError> {
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        // SAFETY: Take ownership before calling: Apple's implementation frees the
        // handle unconditionally, so neither retries nor Drop may release it again.
        check("IOPSReleasePowerSource", unsafe {
            (self.spi.release)(handle.as_ptr())
        })
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        if let Err(error) = self.release() {
            warn!(%error, "failed to release macOS accessory power source");
        }
    }
}

/// Native backend with lazy API lookup and RAII ownership of every source.
#[derive(Default)]
pub struct IoKitPowerSourceBackend {
    sources: BTreeMap<String, Source>,
}

impl IoKitPowerSourceBackend {
    /// Construct an empty backend without loading private APIs.
    pub fn new() -> Self {
        Self::default()
    }
}

impl PowerSourceBackend for IoKitPowerSourceBackend {
    fn prepare(&mut self) -> Result<(), PowerSourceError> {
        spi().map(|_| ())
    }

    fn upsert(&mut self, accessory: &AccessoryPower) -> Result<(), PowerSourceError> {
        let source = match self.sources.entry(accessory.identifier.clone()) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Source::create(spi()?)?)
            }
        };
        // Keep newly created handles after a failed set: the next reconcile can
        // retry details, and disable/offline/shutdown still owns their cleanup.
        source.set(accessory)
    }

    fn remove(&mut self, identifier: &str) -> Result<(), PowerSourceError> {
        self.sources
            .remove(identifier)
            .map_or(Ok(()), |mut source| source.release())
    }
}

fn details_dictionary(accessory: &AccessoryPower) -> CFRetained<CFDictionary<CFString, CFType>> {
    let keys = [
        "Type",
        "Accessory Category",
        "Accessory Identifier",
        "Name",
        "Transport Type",
        "Power Source State",
        "Current Capacity",
        "Max Capacity",
        "Is Present",
        "Is Charging",
    ]
    .map(CFString::from_str);
    let source_type = CFString::from_str("Accessory Source");
    let category = CFString::from_str(accessory.category.as_str());
    let identifier = CFString::from_str(&accessory.identifier);
    let name = CFString::from_str(&accessory.name);
    // The private widget path recognizes Bluetooth accessories. This is display
    // metadata only; no Bluetooth device or connection is created.
    let transport = CFString::from_str("Bluetooth");
    let state = CFString::from_str(
        if matches!(
            accessory.status,
            BatteryStatus::Charging | BatteryStatus::ChargingSlow | BatteryStatus::Full
        ) {
            "AC Power"
        } else {
            "Battery Power"
        },
    );
    let capacity = CFNumber::new_i32(i32::from(accessory.percentage));
    let maximum = CFNumber::new_i32(100);
    let values: [&CFType; 10] = [
        source_type.as_ref(),
        category.as_ref(),
        identifier.as_ref(),
        name.as_ref(),
        transport.as_ref(),
        state.as_ref(),
        capacity.as_ref(),
        maximum.as_ref(),
        CFBoolean::new(true).as_ref(),
        CFBoolean::new(matches!(
            accessory.status,
            BatteryStatus::Charging | BatteryStatus::ChargingSlow
        ))
        .as_ref(),
    ];
    CFDictionary::from_slices(&keys.each_ref().map(|key| &**key), &values)
}

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static RELEASES: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn unused_create(_: *mut *mut c_void) -> i32 {
        -1
    }

    unsafe extern "C" fn unused_set(_: *mut c_void, _: &CFDictionary<CFString, CFType>) -> i32 {
        -1
    }

    unsafe extern "C" fn failed_release(_: *mut c_void) -> i32 {
        RELEASES.fetch_add(1, Ordering::SeqCst);
        -1
    }

    #[test]
    fn native_details_preserve_full_external_power_without_active_charging() {
        for (status, power_source, charging) in [
            (BatteryStatus::Discharging, "Battery Power", false),
            (BatteryStatus::Charging, "AC Power", true),
            (BatteryStatus::ChargingSlow, "AC Power", true),
            (BatteryStatus::Full, "AC Power", false),
        ] {
            let details = details_dictionary(&AccessoryPower {
                identifier: "test".into(),
                name: "Mouse".into(),
                category: super::super::AccessoryCategory::Mouse,
                percentage: 100,
                status,
            });
            let state = details
                .get(&CFString::from_str("Power Source State"))
                .unwrap();
            assert_eq!(
                state.downcast_ref::<CFString>().unwrap().to_string(),
                power_source,
                "{status:?}"
            );
            let active = details.get(&CFString::from_str("Is Charging")).unwrap();
            assert_eq!(
                active.downcast_ref::<CFBoolean>().unwrap().as_bool(),
                charging,
                "{status:?}"
            );
        }
    }

    #[test]
    fn source_drop_does_not_retry_a_failed_consuming_release() {
        static TEST_SPI: Spi = Spi {
            create: unused_create,
            set_details: unused_set,
            release: failed_release,
        };
        let mut source = Source {
            handle: Some(NonNull::dangling()),
            spi: &TEST_SPI,
        };
        let error = source.release().unwrap_err();
        assert_eq!(
            error,
            PowerSourceError::Operation {
                operation: "IOPSReleasePowerSource",
                code: -1
            }
        );
        drop(source);
        assert_eq!(RELEASES.load(Ordering::SeqCst), 1);
        // An owned handle that has not been released still gets RAII cleanup.
        let pending = Source {
            handle: Some(NonNull::dangling()),
            spi: &TEST_SPI,
        };
        let error = pending
            .set(&AccessoryPower {
                identifier: "test".into(),
                name: "Mouse".into(),
                category: super::super::AccessoryCategory::Mouse,
                percentage: 50,
                status: BatteryStatus::Discharging,
            })
            .unwrap_err();
        assert_eq!(
            error,
            PowerSourceError::Operation {
                operation: "IOPSSetPowerSourceDetails",
                code: -1
            }
        );
        drop(pending);
        assert_eq!(RELEASES.load(Ordering::SeqCst), 2);
    }
}
