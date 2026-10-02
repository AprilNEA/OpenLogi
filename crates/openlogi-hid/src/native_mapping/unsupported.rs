use super::{MappingArray, MappingBackend, MappingScope, PeripheralError, ServiceEvidence};

/// Explicitly unavailable native mapping service on this host.
#[derive(Default)]
pub struct NativeMapping;

impl MappingBackend for NativeMapping {
    fn read(
        &mut self,
        _: MappingScope,
    ) -> impl Future<Output = Result<ServiceEvidence, PeripheralError>> {
        std::future::ready(Err(PeripheralError::Unsupported(
            "native HID mapping requires macOS".into(),
        )))
    }

    fn write(
        &mut self,
        _: MappingScope,
        _: &MappingArray,
    ) -> impl Future<Output = Result<(), PeripheralError>> {
        std::future::ready(Err(PeripheralError::Unsupported(
            "native HID mapping requires macOS".into(),
        )))
    }
}
