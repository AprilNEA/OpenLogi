//! Shared asset registry types + HTTP fetch helpers for OpenLogi's asset mirrors.
//!
//! Consumers:
//!
//! - `openlogi` (CLI): bulk-pulls the whole registry at packaging time
//!   (`openlogi assets sync`).
//! - `openlogi-gui`: pulls only the connected device's files at startup
//!   (runtime safety net + dev convenience).
//!
//! No filesystem layout opinions live here — both consumers decide where
//! files end up. This crate stays I/O-light: parsing, HTTP, hashing.

mod error;
pub mod http;
pub mod index;
pub mod manifest;
pub mod metadata;
mod source;

pub use error::AssetError;
pub use http::{AssetClient, FetchOutcome, cached_matches, read_bytes, sha256_hex, sha256_of_file};
pub use index::{
    BUTTONS_RENDER_FILES, DeviceEntry, FRONT_RENDER_FILES, FileEntry, Index, METADATA_FILES,
};
pub use manifest::{DepotManifest, ManifestDevice, ManifestResource, variant_model_id};
pub use metadata::{Assignment, Direction, ImageEntry, Metadata, Origin, Point};
pub use source::{AssetRegistry, AssetSource};
