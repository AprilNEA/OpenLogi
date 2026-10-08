use std::collections::BTreeMap;
use std::process::Stdio;
use std::time::Duration;

use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFNumber, CFString, CFType};
use objc2_io_kit::{IOHIDEventSystemClient, IOHIDServiceClient};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::Command;

use super::{MappingArray, MappingBackend, MappingScope, PeripheralError, ServiceEvidence};

const OUTPUT_LIMIT: u64 = 1024 * 1024;
const COMMAND_DEADLINE: Duration = Duration::from_secs(5);

/// macOS's scoped UserKeyMapping service, with typed property reads.
#[derive(Default)]
pub struct NativeMapping;

impl MappingBackend for NativeMapping {
    async fn read(&mut self, scope: MappingScope) -> Result<ServiceEvidence, PeripheralError> {
        let before = service_arrays(scope)?;
        let boot = run("/usr/sbin/sysctl", &["-n", "kern.boottime"]).await?;
        let services = service_arrays(scope)?;
        if before.keys().ne(services.keys()) {
            return Err(PeripheralError::StaleSession);
        }
        Ok(ServiceEvidence {
            boot: boot.trim().into(),
            services,
        })
    }

    async fn write(
        &mut self,
        scope: MappingScope,
        mappings: &MappingArray,
    ) -> Result<(), PeripheralError> {
        let matching =
            serde_json::json!({"VendorID": scope.vendor_id, "ProductID": scope.product_id})
                .to_string();
        let property = serde_json::json!({"UserKeyMapping": mappings}).to_string();
        run(
            "/usr/bin/hidutil",
            &["property", "--matching", &matching, "--set", &property],
        )
        .await
        .map_err(|error| PeripheralError::WriteFailed(error.to_string()))?;
        Ok(())
    }
}

#[expect(
    unsafe_code,
    reason = "IOHIDEventSystemClientCopyServices documents an array of retained IOHIDServiceClient objects"
)]
fn service_arrays(scope: MappingScope) -> Result<BTreeMap<u64, MappingArray>, PeripheralError> {
    let client = IOHIDEventSystemClient::new_simple_client(None);
    let services = client.services().ok_or_else(|| {
        PeripheralError::DiscoveryUnavailable("HID event services could not be read".into())
    })?;
    // SAFETY: IOHIDEventSystemClientCopyServices returns IOHIDServiceClientRefs, retained by this array.
    let services = unsafe { services.cast_unchecked::<IOHIDServiceClient>() };
    let mut arrays = BTreeMap::new();
    for service in services {
        if number_property(&service, "VendorID") != Some(u64::from(scope.vendor_id))
            || number_property(&service, "ProductID") != Some(u64::from(scope.product_id))
        {
            continue;
        }
        let id = unsigned_number(&service.registry_id())
            .ok_or_else(|| PeripheralError::ReadFailed("invalid HID service registry ID".into()))?;
        let mappings = match service.property(&CFString::from_str("UserKeyMapping")) {
            Some(value) => serde_json::from_value(cf_json(&value, 0)?)
                .map_err(|error| PeripheralError::ReadFailed(format!("UserKeyMapping: {error}")))?,
            None => MappingArray::default(),
        };
        if arrays.insert(id, mappings).is_some() {
            return Err(PeripheralError::MappingScopeConflict);
        }
    }
    if arrays.is_empty() {
        return Err(PeripheralError::Offline);
    }
    Ok(arrays)
}

fn unsigned_number(value: &CFType) -> Option<u64> {
    value.downcast_ref::<CFNumber>()?.as_i64()?.try_into().ok()
}

fn number_property(service: &IOHIDServiceClient, key: &str) -> Option<u64> {
    let property = service.property(&CFString::from_str(key))?;
    unsigned_number(&property)
}

#[expect(
    unsafe_code,
    reason = "IOHIDServiceClientCopyProperty returns property-list containers; each nested value is type-checked"
)]
fn cf_json(value: &CFType, depth: usize) -> Result<Value, PeripheralError> {
    let invalid =
        || PeripheralError::ReadFailed("native property cannot be preserved as JSON".into());
    if depth > 16 {
        return Err(invalid());
    }
    if let Some(value) = value.downcast_ref::<CFBoolean>() {
        return Ok(Value::Bool(value.as_bool()));
    }
    if let Some(value) = value.downcast_ref::<CFNumber>() {
        return if value.is_float_type() {
            value
                .as_f64()
                .and_then(serde_json::Number::from_f64)
                .map(Value::Number)
                .ok_or_else(invalid)
        } else {
            value.as_i64().map(Value::from).ok_or_else(invalid)
        };
    }
    if let Some(value) = value.downcast_ref::<CFString>() {
        return Ok(Value::String(value.to_string()));
    }
    if let Some(value) = value.downcast_ref::<CFArray>() {
        if value.len() > 4096 {
            return Err(invalid());
        }
        // SAFETY: This array is part of the property list returned by the HID service.
        let values = unsafe { value.cast_unchecked::<CFType>() };
        return values
            .iter()
            .map(|value| cf_json(&value, depth + 1))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array);
    }
    if let Some(value) = value.downcast_ref::<CFDictionary>() {
        if value.len() > 4096 {
            return Err(invalid());
        }
        // SAFETY: HID property-list dictionaries contain CFType keys and values, retained by the copied property.
        let dictionary = unsafe { value.cast_unchecked::<CFType, CFType>() };
        let (keys, values) = dictionary.to_vecs();
        let mut fields = serde_json::Map::new();
        for (key, value) in keys.iter().zip(&values) {
            let key = key
                .downcast_ref::<CFString>()
                .ok_or_else(invalid)?
                .to_string();
            fields.insert(key, cf_json(value, depth + 1)?);
        }
        return Ok(Value::Object(fields));
    }
    Err(invalid())
}

async fn read_bounded(reader: impl AsyncRead + Unpin) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(OUTPUT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() as u64 > OUTPUT_LIMIT {
        return Err(std::io::Error::other("native command output exceeds 1 MiB"));
    }
    Ok(bytes)
}

async fn run(program: &str, arguments: &[&str]) -> Result<String, PeripheralError> {
    let mut child = Command::new(program)
        .args(arguments)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| PeripheralError::ReadFailed(format!("{program}: {error}")))?;
    let operation = async {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("native command stdout unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("native command stderr unavailable"))?;
        tokio::try_join!(child.wait(), read_bounded(stdout), read_bounded(stderr))
    };
    let (status, stdout, stderr) = tokio::time::timeout(COMMAND_DEADLINE, operation)
        .await
        .map_err(|_| PeripheralError::ReadFailed(format!("{program} timed out")))?
        .map_err(|error| PeripheralError::ReadFailed(format!("{program}: {error}")))?;
    if !status.success() {
        return Err(PeripheralError::ReadFailed(format!(
            "{program} ({status}): {}",
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    String::from_utf8(stdout)
        .map_err(|error| PeripheralError::ReadFailed(format!("{program}: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_read_keeps_numbers_strings_and_boolean_fields_distinct() {
        let number = CFNumber::new_i64(30_064_771_181);
        let string = CFString::from_str("30064771181");
        let boolean = CFBoolean::new(true);
        let array = CFArray::<CFType>::from_objects(&[&number, &string, boolean]);
        assert_eq!(
            cf_json(&array, 0).unwrap(),
            serde_json::json!([30_064_771_181_i64, "30064771181", true])
        );
    }
}
