// Copyright 2018 Google LLC
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::io;

/// Serializes [`io::ErrorKind`] as a `u32`.
#[allow(clippy::trivially_copy_pass_by_ref)] // Exact fn signature required by serde derive
pub fn serialize_io_error_kind_as_u32<S>(
    kind: &io::ErrorKind,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    use std::io::ErrorKind::*;
    match *kind {
        NotFound => 0,
        PermissionDenied => 1,
        ConnectionRefused => 2,
        ConnectionReset => 3,
        ConnectionAborted => 4,
        NotConnected => 5,
        AddrInUse => 6,
        AddrNotAvailable => 7,
        BrokenPipe => 8,
        AlreadyExists => 9,
        WouldBlock => 10,
        InvalidInput => 11,
        InvalidData => 12,
        TimedOut => 13,
        WriteZero => 14,
        Interrupted => 15,
        Other => 16,
        UnexpectedEof => 17,
        _ => 16,
    }
    .serialize(serializer)
}

/// Deserializes [`io::ErrorKind`] from a `u32`.
pub fn deserialize_io_error_kind_from_u32<'de, D>(
    deserializer: D,
) -> Result<io::ErrorKind, D::Error>
where
    D: Deserializer<'de>,
{
    use std::io::ErrorKind::*;
    Ok(match u32::deserialize(deserializer)? {
        0 => NotFound,
        1 => PermissionDenied,
        2 => ConnectionRefused,
        3 => ConnectionReset,
        4 => ConnectionAborted,
        5 => NotConnected,
        6 => AddrInUse,
        7 => AddrNotAvailable,
        8 => BrokenPipe,
        9 => AlreadyExists,
        10 => WouldBlock,
        11 => InvalidInput,
        12 => InvalidData,
        13 => TimedOut,
        14 => WriteZero,
        15 => Interrupted,
        16 => Other,
        17 => UnexpectedEof,
        _ => Other,
    })
}
