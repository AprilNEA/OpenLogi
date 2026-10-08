//! Exact endpoint selectors. Selection order is based on set inclusion.

use openlogi_core::peripheral::{ControlId, Endpoint, Transport};
use serde::{Deserialize, Serialize};

use crate::PluginError;

/// One AND clause; clauses for the same role are alternatives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selector {
    /// Required endpoint role, scoped to its descriptor.
    pub role: ControlId,
    /// Host-supported transport.
    pub transport: Transport,
    /// Exact manufacturer identifier.
    pub vendor_id: u16,
    /// Exact product identifier.
    pub product_id: u16,
    /// Exact HID usage page; required for HID transports.
    pub usage_page: Option<u16>,
    /// Exact HID usage; required for HID transports.
    pub usage: Option<u16>,
    /// Optional exact USB interface constraint.
    pub interface: Option<u8>,
    /// Optional exact input report constraint.
    pub report_id: Option<u8>,
}

impl Selector {
    /// Validate cross-field transport requirements.
    pub fn validate(&self) -> Result<(), PluginError> {
        match self.transport {
            Transport::UsbHid | Transport::BluetoothHid
                if self.usage_page.is_some() && self.usage.is_some() => Ok(()),
            Transport::Uvc
                if self.usage_page.is_none() && self.usage.is_none() && self.report_id.is_none() => Ok(()),
            _ => Err(PluginError::Invalid("HID selectors require usage_page and usage; UVC selectors cannot contain HID fields".into())),
        }
    }

    /// Match only known facts. Missing metadata never satisfies an exact constraint.
    #[must_use]
    pub fn matches(&self, endpoint: &Endpoint) -> bool {
        endpoint.transport == Some(self.transport)
            && endpoint.vendor_id == self.vendor_id
            && endpoint.product_id == self.product_id
            && self
                .usage_page
                .is_none_or(|page| endpoint.collection.is_some_and(|u| u.page == page))
            && self
                .usage
                .is_none_or(|usage| endpoint.collection.is_some_and(|u| u.usage == usage))
            && self
                .interface
                .is_none_or(|interface| endpoint.interface == Some(interface))
            && self
                .report_id
                .is_none_or(|id| endpoint.report_ids.contains(&id))
    }

    /// Whether every endpoint accepted here is also accepted by `other`.
    #[must_use]
    pub fn is_subset_of(&self, other: &Self) -> bool {
        self.role == other.role
            && self.transport == other.transport
            && self.vendor_id == other.vendor_id
            && self.product_id == other.product_id
            && other
                .usage_page
                .is_none_or(|value| self.usage_page == Some(value))
            && other.usage.is_none_or(|value| self.usage == Some(value))
            && other
                .interface
                .is_none_or(|value| self.interface == Some(value))
            && other
                .report_id
                .is_none_or(|value| self.report_id == Some(value))
    }
}
