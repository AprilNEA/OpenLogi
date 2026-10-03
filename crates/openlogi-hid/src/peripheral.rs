//! Host-discovered HID endpoints and scoped report I/O for selected drivers.

use std::{collections::BTreeSet, sync::Arc};

use async_hid::{
    AsyncHidFeatureHandle as _, AsyncHidRead as _, AsyncHidWrite as _, Device, DeviceFeatureHandle,
    DeviceReader, DeviceWriter,
};
use openlogi_core::peripheral::{Endpoint, EndpointId, HidUsage, PeripheralError, Transport};

use crate::DeviceIoGate;

#[cfg(any(target_os = "linux", all(test, unix)))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "windows")]
use windows as platform;

/// One collection and its host handle, before any device open or probe.
#[derive(Clone)]
pub struct DiscoveredEndpoint {
    /// Sanitized facts used by the catalog.
    pub endpoint: Endpoint,
    /// Host product label.
    pub name: String,
    /// Protected built-in transport owner, including receiver sibling collections.
    pub builtin_owner: Option<&'static str>,
    device: Arc<Device>,
}

impl std::fmt::Debug for DiscoveredEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.endpoint.fmt(f)
    }
}

/// Supplementary host facts, never inferred from VID/PID or an unverified serial.
#[derive(Default)]
struct Metadata {
    parent: Option<EndpointId>,
    transport: Option<Transport>,
    interface: Option<u8>,
    max_report_bytes: Option<u32>,
    max_output_report_bytes: Option<u32>,
    max_feature_report_bytes: Option<u32>,
}

/// Discover through the existing process-wide HID manager without opening a device.
pub async fn discover(gate: &DeviceIoGate) -> Result<Vec<DiscoveredEndpoint>, PeripheralError> {
    gate.ensure_allowed()
        .map_err(|e| PeripheralError::DiscoveryUnavailable(e.to_string()))?;
    let devices = crate::transport::enumerate_devices()
        .await
        .map_err(|e| PeripheralError::DiscoveryUnavailable(e.to_string()))?;
    let metadata = platform::metadata(&devices)?;
    let group = |device: &Device| {
        let node = format!("{:?}", device.id);
        metadata
            .get(&node)
            .and_then(|m| m.parent.clone())
            .unwrap_or(EndpointId(node))
    };
    let protected: BTreeSet<_> = devices
        .iter()
        .filter(|d| crate::transport::is_hidpp_node(d))
        .map(group)
        .collect();
    let mut result = Vec::new();
    for device in devices {
        let node = format!("{:?}", device.id);
        let extra = metadata.get(&node);
        let builtin_owner = if protected.contains(&group(&device)) {
            Some(openlogi_device_registry::HIDPP_DRIVER_ID)
        } else {
            openlogi_device_registry::litra::find_litra(
                device.vendor_id,
                device.product_id,
                device.usage_page,
                device.usage_id,
            )
            .map(|d| d.driver_id)
        };
        let endpoint = Endpoint {
            id: EndpointId(format!(
                "{node}/{:04x}/{:04x}",
                device.usage_page, device.usage_id
            )),
            parent: Some(group(&device)),
            transport: extra.and_then(|m| m.transport),
            vendor_id: device.vendor_id,
            product_id: device.product_id,
            collection: Some(HidUsage {
                page: device.usage_page,
                usage: device.usage_id,
            }),
            interface: extra.and_then(|m| m.interface),
            report_ids: Vec::new(),
            max_report_bytes: extra.and_then(|m| m.max_report_bytes),
            max_output_report_bytes: extra.and_then(|m| m.max_output_report_bytes),
            max_feature_report_bytes: extra.and_then(|m| m.max_feature_report_bytes),
        };
        result.push(DiscoveredEndpoint {
            endpoint,
            name: device.name.clone(),
            builtin_owner,
            device: Arc::new(device),
        });
    }
    gate.ensure_allowed()
        .map_err(|e| PeripheralError::DiscoveryUnavailable(e.to_string()))?;
    Ok(result)
}

/// Report framing at the host boundary: the payload excludes the ID byte.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// Zero denotes an unnumbered report.
    pub id: u8,
    /// Report bytes excluding the ID.
    pub payload: Vec<u8>,
}

/// Access modes are opened separately after catalog selection and claim acquisition.
pub struct ReportDevice {
    device: Arc<Device>,
    gate: DeviceIoGate,
    writer: Option<DeviceWriter>,
    features: Option<DeviceFeatureHandle>,
}

impl DiscoveredEndpoint {
    /// The protocol implementation for this collection, excluding receiver siblings.
    #[must_use]
    pub fn protocol_driver(&self) -> Option<openlogi_device_registry::driver::BuiltinDriver> {
        use openlogi_device_registry::driver::BuiltinDriver;
        let driver = BuiltinDriver::for_hid(
            self.endpoint.vendor_id,
            self.endpoint.product_id,
            self.device.usage_page,
            self.device.usage_id,
        )?;
        (driver != BuiltinDriver::Hidpp || crate::transport::is_hidpp_node(&self.device))
            .then_some(driver)
    }

    /// Reuse the host backend's exact node and route identity during driver handoff.
    #[must_use]
    pub fn node(&self) -> crate::NodeInfo {
        crate::transport::node_info(&self.device)
    }

    /// Bind I/O to this exact discovered node and the host lifecycle gate.
    #[must_use]
    pub fn report_device(&self, gate: DeviceIoGate) -> ReportDevice {
        ReportDevice {
            device: Arc::clone(&self.device),
            gate,
            writer: None,
            features: None,
        }
    }
}

impl ReportDevice {
    /// Open a nonexclusive input subscription after the host validates its grant.
    pub async fn input(&self, numbered: bool) -> Result<ReportInput, PeripheralError> {
        self.allowed()?;
        let reader = self
            .device
            .open_readable()
            .await
            .map_err(|e| read_error(&e))?;
        Ok(ReportInput {
            reader,
            gate: self.gate.clone(),
            numbered,
        })
    }

    /// Write an output report exactly once. The broker owns deadlines and drains native work.
    pub async fn output(
        &mut self,
        report: &Report,
        check: impl Fn() -> Result<(), PeripheralError>,
    ) -> Result<(), PeripheralError> {
        self.allowed()?;
        if self.writer.is_none() {
            self.writer = Some(
                self.device
                    .open_writeable()
                    .await
                    .map_err(|e| write_error(&e))?,
            );
        }
        let bytes = encode(report);
        self.allowed()?;
        check()?;
        if let Some(writer) = self.writer.as_mut() {
            writer
                .write_output_report(&bytes)
                .await
                .map_err(|e| write_error(&e))?;
        }
        Ok(())
    }

    /// Read a granted feature report, preserving numbered versus unnumbered framing.
    pub async fn feature_read(
        &mut self,
        id: u8,
        bytes: usize,
        check: impl Fn() -> Result<(), PeripheralError> + Send + 'static,
    ) -> Result<Report, PeripheralError> {
        self.allowed()?;
        check()?;
        if !(1..=65536).contains(&bytes) {
            return Err(PeripheralError::InvalidSettings(
                "feature report length must be 1–65536 bytes".into(),
            ));
        }
        #[cfg(target_os = "macos")]
        {
            macos::feature_read(self.device.id.clone(), id, bytes, check).await
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.feature_handle().await?;
            check()?;
            let mut buffer = vec![0; bytes];
            buffer[0] = id;
            let Some(features) = self.features.as_mut() else {
                return Err(PeripheralError::ReadFailed(
                    "feature handle unavailable".into(),
                ));
            };
            let count = features
                .read_feature_report(&mut buffer)
                .await
                .map_err(|e| read_error(&e))?;
            decode(&buffer[..count], true)
        }
    }

    /// Write a feature report exactly once through the selected endpoint.
    pub async fn feature_write(
        &mut self,
        report: &Report,
        check: impl Fn() -> Result<(), PeripheralError>,
    ) -> Result<(), PeripheralError> {
        self.feature_handle().await?;
        check()?;
        let bytes = encode(report);
        let Some(features) = self.features.as_mut() else {
            return Err(PeripheralError::WriteFailed(
                "feature handle unavailable".into(),
            ));
        };
        features
            .write_feature_report(&bytes)
            .await
            .map_err(|e| write_error(&e))
    }

    async fn feature_handle(&mut self) -> Result<(), PeripheralError> {
        self.allowed()?;
        if self.features.is_none() {
            self.features = Some(
                self.device
                    .open_feature_handle()
                    .await
                    .map_err(|e| read_error(&e))?,
            );
        }
        self.allowed()
    }

    fn allowed(&self) -> Result<(), PeripheralError> {
        self.gate
            .ensure_allowed()
            .map_err(|e| PeripheralError::PermissionDenied(e.to_string()))
    }
}

/// A session-owned reader; dropping the reader cancels the native subscription.
pub struct ReportInput {
    reader: DeviceReader,
    gate: DeviceIoGate,
    numbered: bool,
}

impl ReportInput {
    /// Read one complete host-bounded report. The broker validates the narrower grant.
    pub async fn next(&mut self) -> Result<Report, PeripheralError> {
        self.gate
            .ensure_allowed()
            .map_err(|e| PeripheralError::PermissionDenied(e.to_string()))?;
        let mut bytes = vec![0; 65537];
        let count = self
            .reader
            .read_input_report(&mut bytes)
            .await
            .map_err(|e| read_error(&e))?;
        if count > 65536 {
            return Err(PeripheralError::ResourceLimit(
                "input report exceeds 65536 bytes".into(),
            ));
        }
        decode(&bytes[..count], self.numbered)
    }
}

fn encode(report: &Report) -> Vec<u8> {
    std::iter::once(report.id)
        .chain(report.payload.iter().copied())
        .collect()
}

fn decode(bytes: &[u8], numbered: bool) -> Result<Report, PeripheralError> {
    if numbered {
        let (&id, payload) = bytes
            .split_first()
            .ok_or_else(|| PeripheralError::ReadFailed("empty numbered HID report".into()))?;
        Ok(Report {
            id,
            payload: payload.into(),
        })
    } else {
        Ok(Report {
            id: 0,
            payload: bytes.into(),
        })
    }
}

fn read_error(error: &async_hid::HidError) -> PeripheralError {
    PeripheralError::ReadFailed(error.to_string())
}
fn write_error(error: &async_hid::HidError) -> PeripheralError {
    PeripheralError::WriteFailed(error.to_string())
}
