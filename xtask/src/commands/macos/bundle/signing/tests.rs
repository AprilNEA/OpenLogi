use plist::Value;

use super::camera_entitlements_path;
use crate::support::fs::repo_root;

#[test]
fn camera_entitlements_declare_device_camera() {
    let path = camera_entitlements_path(&repo_root().unwrap());
    let plist = Value::from_file(&path).unwrap();
    let dict = plist.as_dictionary().unwrap();
    assert_eq!(
        dict.get("com.apple.security.device.camera")
            .and_then(Value::as_boolean),
        Some(true),
        "hardened-runtime camera capture needs this entitlement"
    );
}
