//! Short authentication string derivation for Flow pairing.

use std::fmt;

use hkdf::Hkdf;
use sha2::Sha256;
use thiserror::Error;

const SAS_INFO: &[u8] = b"openlogi-flow-sas-v1";

/// A validated 32-byte Ed25519 public key.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PublicKey([u8; 32]);

impl PublicKey {
    /// Creates a public key from its canonical 32-byte representation.
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the canonical key bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl From<[u8; 32]> for PublicKey {
    fn from(value: [u8; 32]) -> Self {
        Self::new(value)
    }
}

impl TryFrom<&[u8]> for PublicKey {
    type Error = FixedBytesError;

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        <[u8; 32]>::try_from(value)
            .map(Self)
            .map_err(|_| FixedBytesError::new(32, value.len()))
    }
}

/// A validated 16-byte nonce that uniquely identifies a pairing session.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionNonce([u8; 16]);

impl SessionNonce {
    /// Creates a session nonce from its canonical 16-byte representation.
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Returns the nonce bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl From<[u8; 16]> for SessionNonce {
    fn from(value: [u8; 16]) -> Self {
        Self::new(value)
    }
}

impl TryFrom<&[u8]> for SessionNonce {
    type Error = FixedBytesError;

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        <[u8; 16]>::try_from(value)
            .map(Self)
            .map_err(|_| FixedBytesError::new(16, value.len()))
    }
}

/// An error converting bytes into a fixed-width Flow value.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("expected {expected} bytes, received {actual}")]
pub struct FixedBytesError {
    expected: usize,
    actual: usize,
}

impl FixedBytesError {
    const fn new(expected: usize, actual: usize) -> Self {
        Self { expected, actual }
    }

    /// Returns the required byte count.
    #[must_use]
    pub const fn expected(self) -> usize {
        self.expected
    }

    /// Returns the supplied byte count.
    #[must_use]
    pub const fn actual(self) -> usize {
        self.actual
    }
}

/// A six-decimal-digit Flow short authentication string.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SasCode(u32);

impl SasCode {
    /// Returns the numeric code in the range `0..1_000_000`.
    #[must_use]
    pub const fn value(self) -> u32 {
        self.0
    }
}

impl fmt::Display for SasCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:06}", self.0)
    }
}

/// Derives the symmetric Flow SAS from two peers' public keys and session nonces.
///
/// Keys and nonces are independently ordered byte-lexicographically, making the
/// result independent of which peer initiates the connection.
#[must_use]
pub fn derive_sas(
    first_key: PublicKey,
    second_key: PublicKey,
    first_nonce: SessionNonce,
    second_nonce: SessionNonce,
) -> SasCode {
    let (low_key, high_key) = ordered(first_key.as_bytes(), second_key.as_bytes());
    let (low_nonce, high_nonce) = ordered(first_nonce.as_bytes(), second_nonce.as_bytes());

    let mut ikm = [0_u8; 64];
    ikm[..32].copy_from_slice(low_key);
    ikm[32..].copy_from_slice(high_key);
    let mut salt = [0_u8; 32];
    salt[..16].copy_from_slice(low_nonce);
    salt[16..].copy_from_slice(high_nonce);

    let hkdf = Hkdf::<Sha256>::new(Some(&salt), &ikm);
    let mut output = [0_u8; 4];
    // Four bytes are always below HKDF-SHA256's 8160-byte output limit.
    hkdf.expand(SAS_INFO, &mut output)
        .unwrap_or_else(|_| unreachable!("four-byte HKDF output is always valid"));
    SasCode(u32::from_be_bytes(output) % 1_000_000)
}

fn ordered<'a, T: Ord>(first: &'a T, second: &'a T) -> (&'a T, &'a T) {
    if first <= second {
        (first, second)
    } else {
        (second, first)
    }
}

#[cfg(test)]
mod tests {
    use super::{PublicKey, SessionNonce, derive_sas};

    #[test]
    fn fixed_protocol_vector() {
        let first_key = PublicKey::new(core::array::from_fn(|index| u8::try_from(index).unwrap()));
        let second_key = PublicKey::new(core::array::from_fn(|index| {
            u8::try_from(index + 32).unwrap()
        }));
        let first_nonce =
            SessionNonce::new(core::array::from_fn(|index| u8::try_from(index).unwrap()));
        let second_nonce = SessionNonce::new(core::array::from_fn(|index| {
            u8::try_from(index + 16).unwrap()
        }));

        assert_eq!(
            derive_sas(first_key, second_key, first_nonce, second_nonce).to_string(),
            "136751"
        );
    }

    #[test]
    fn swapping_peers_preserves_code() {
        let first_key = PublicKey::new([0x55; 32]);
        let second_key = PublicKey::new([0xaa; 32]);
        let first_nonce = SessionNonce::new([0x11; 16]);
        let second_nonce = SessionNonce::new([0xee; 16]);

        assert_eq!(
            derive_sas(first_key, second_key, first_nonce, second_nonce),
            derive_sas(second_key, first_key, second_nonce, first_nonce)
        );
    }
}
