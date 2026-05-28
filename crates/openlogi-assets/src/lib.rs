//! Shared asset registry types + HTTP fetch helpers for assets.openlogi.org.
//!
//! Consumers:
//!
//! - `openlogi-cli`: bulk-pulls the whole registry at packaging time
//!   (`openlogi assets sync`).
//! - `openlogi-gui`: pulls only the connected device's files at startup
//!   (runtime safety net + dev convenience).
//!
//! No filesystem layout opinions live here — both consumers decide where
//! files end up. This crate stays I/O-light: parsing, HTTP, hashing.

pub mod http;
pub mod index;
pub mod manifest;
pub mod metadata;

pub use http::{
    cached_matches, fetch_file, fetch_file_to_dir, fetch_index, fetch_index_raw,
    fetch_index_to_dir, read_bytes, sha256_hex, sha256_of_file,
};
pub use index::{DeviceEntry, FileEntry, Index};
pub use manifest::{DepotManifest, ManifestDevice, ManifestResource, variant_model_id};
pub use metadata::{Assignment, Direction, ImageEntry, Metadata, Origin, Point};
