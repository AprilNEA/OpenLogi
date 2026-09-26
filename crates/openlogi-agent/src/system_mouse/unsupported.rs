//! Fallback for operating systems without a system mouse backend yet.

use openlogi_ipc::{PrimaryMouseButton, SystemMouseSettingError};

use super::Backend;

pub(super) struct UnsupportedBackend;

impl Backend for UnsupportedBackend {
    const NAME: &'static str = "unsupported";

    fn is_available() -> bool {
        false
    }

    fn read() -> Result<PrimaryMouseButton, SystemMouseSettingError> {
        Err(SystemMouseSettingError::Unsupported)
    }

    fn set(_: PrimaryMouseButton) -> Result<PrimaryMouseButton, SystemMouseSettingError> {
        Err(SystemMouseSettingError::Unsupported)
    }
}
