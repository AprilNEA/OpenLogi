//! Pairing-nonce commitments and short authentication string derivation.
//!
//! The pairing ceremony is commit-then-reveal (FLOW-PROTOCOL.md §Pairing):
//! the initiator commits to its nonce, the responder answers with its own
//! nonce, and the initiator then opens the commitment. Neither side can choose
//! its nonce after seeing the other one, so an active attacker who terminates
//! two sessions cannot steer them to the same code; each attempt matches with
//! probability 10⁻⁶.

use std::fmt;

use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;

const COMMIT_DOMAIN: &[u8] = b"openlogi-flow-pair-commit-v1";
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
        fixed(value).map(Self)
    }
}

/// The 16-byte `Hello.session_nonce`, fresh on every connection.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionNonce([u8; 16]);

impl SessionNonce {
    /// Creates a session nonce from its canonical 16-byte representation.
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Draws a session nonce from the operating system's secure generator.
    pub fn random() -> Result<Self, RandomError> {
        random_bytes().map(Self)
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
        fixed(value).map(Self)
    }
}

/// One side's 16-byte pairing nonce.
///
/// The initiator's nonce stays secret until the responder has sent its own;
/// both are discarded when the ceremony ends.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct PairingNonce([u8; 16]);

impl fmt::Debug for PairingNonce {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PairingNonce([REDACTED])")
    }
}

impl PairingNonce {
    /// Creates a pairing nonce from 16 bytes.
    ///
    /// Production callers use [`Self::random`]; fixed values are for tests and
    /// protocol vectors.
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Draws a pairing nonce from the operating system's secure generator.
    pub fn random() -> Result<Self, RandomError> {
        random_bytes().map(Self)
    }

    /// Returns the nonce bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl TryFrom<&[u8]> for PairingNonce {
    type Error = FixedBytesError;

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        fixed(value).map(Self)
    }
}

/// The initiator's SHA-256 commitment to its pairing nonce.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NonceCommitment([u8; 32]);

impl NonceCommitment {
    /// Commits the initiator to `nonce` for this ordered pair of identities.
    ///
    /// `c = SHA-256("openlogi-flow-pair-commit-v1" ‖ pk_initiator ‖ pk_responder ‖ nonce)`.
    #[must_use]
    pub fn commit(initiator: PublicKey, responder: PublicKey, nonce: PairingNonce) -> Self {
        let digest = Sha256::new()
            .chain_update(COMMIT_DOMAIN)
            .chain_update(initiator.as_bytes())
            .chain_update(responder.as_bytes())
            .chain_update(nonce.as_bytes())
            .finalize();
        Self(digest.into())
    }

    /// Returns whether `nonce` opens this commitment, in constant time.
    #[must_use]
    pub fn opens_to(
        &self,
        initiator: PublicKey,
        responder: PublicKey,
        nonce: PairingNonce,
    ) -> bool {
        bool::from(Self::commit(initiator, responder, nonce).0.ct_eq(&self.0))
    }

    /// Returns the commitment bytes carried in `PairStart.nonce_commitment`.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl TryFrom<&[u8]> for NonceCommitment {
    type Error = FixedBytesError;

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        fixed(value).map(Self)
    }
}

/// SHA-256 of the exact `Hello` payload bytes one side sent on the control stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HelloDigest([u8; 32]);

impl HelloDigest {
    /// Hashes one encoded `Hello` payload.
    #[must_use]
    pub fn of(hello_payload: &[u8]) -> Self {
        Self(Sha256::digest(hello_payload).into())
    }

    /// Returns the digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Everything one role contributes to the pairing code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PairingParty {
    /// The TLS-authenticated Ed25519 identity.
    pub key: PublicKey,
    /// Digest of the `Hello` payload this party sent.
    pub hello: HelloDigest,
    /// This party's pairing nonce.
    pub nonce: PairingNonce,
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

/// The operating system's secure random generator failed.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("secure random generator failed")]
pub struct RandomError;

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

/// Derives the Flow SAS in role order.
///
/// `HKDF-SHA256(salt = h_I ‖ h_R, ikm = pk_I ‖ pk_R ‖ n_I ‖ n_R,
/// info = "openlogi-flow-sas-v1")`, first four output bytes big-endian,
/// `mod 1_000_000`. Both peers know who sent `PairStart`, so they assign the
/// roles identically and compute the same code.
#[must_use]
pub fn derive_sas(initiator: &PairingParty, responder: &PairingParty) -> SasCode {
    let mut salt = [0_u8; 64];
    salt[..32].copy_from_slice(initiator.hello.as_bytes());
    salt[32..].copy_from_slice(responder.hello.as_bytes());
    let mut ikm = [0_u8; 96];
    ikm[..32].copy_from_slice(initiator.key.as_bytes());
    ikm[32..64].copy_from_slice(responder.key.as_bytes());
    ikm[64..80].copy_from_slice(initiator.nonce.as_bytes());
    ikm[80..].copy_from_slice(responder.nonce.as_bytes());

    let hkdf = Hkdf::<Sha256>::new(Some(&salt), &ikm);
    let mut output = [0_u8; 4];
    // Four bytes are always below HKDF-SHA256's 8160-byte output limit.
    hkdf.expand(SAS_INFO, &mut output)
        .unwrap_or_else(|_| unreachable!("four-byte HKDF output is always valid"));
    SasCode(u32::from_be_bytes(output) % 1_000_000)
}

fn fixed<const N: usize>(value: &[u8]) -> Result<[u8; N], FixedBytesError> {
    <[u8; N]>::try_from(value).map_err(|_| FixedBytesError::new(N, value.len()))
}

fn random_bytes<const N: usize>() -> Result<[u8; N], RandomError> {
    let mut bytes = [0_u8; N];
    rustls::crypto::ring::default_provider()
        .secure_random
        .fill(&mut bytes)
        .map_err(|_| RandomError)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counting<const N: usize>(start: usize) -> [u8; N] {
        core::array::from_fn(|index| u8::try_from(index + start).unwrap())
    }

    fn party(key: usize, hello: &[u8], nonce: usize) -> PairingParty {
        PairingParty {
            key: PublicKey::new(counting(key)),
            hello: HelloDigest::of(hello),
            nonce: PairingNonce::new(counting(nonce)),
        }
    }

    #[test]
    fn fixed_protocol_vector() {
        use std::fmt::Write as _;

        let initiator = party(0, b"initiator hello", 0);
        let responder = party(32, b"responder hello", 16);
        let commitment = NonceCommitment::commit(initiator.key, responder.key, initiator.nonce);
        let commitment_hex = commitment
            .as_bytes()
            .iter()
            .fold(String::new(), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            });

        assert_eq!(
            commitment_hex,
            "8d89a09a832cc87779e32053024ef40dbe98bacaacefb05ff934eda83902b31f"
        );
        assert_eq!(derive_sas(&initiator, &responder).to_string(), "469603");
    }

    #[test]
    fn roles_are_part_of_the_code() {
        let first = party(0, b"first hello", 0);
        let second = party(32, b"second hello", 16);

        assert_ne!(derive_sas(&first, &second), derive_sas(&second, &first));
    }

    #[test]
    fn every_input_changes_the_code() {
        let initiator = party(0, b"initiator hello", 0);
        let responder = party(32, b"responder hello", 16);
        let code = derive_sas(&initiator, &responder);
        let variants = [
            PairingParty {
                key: PublicKey::new([0xee; 32]),
                ..initiator
            },
            PairingParty {
                hello: HelloDigest::of(b"other hello"),
                ..initiator
            },
            PairingParty {
                nonce: PairingNonce::new([0xee; 16]),
                ..initiator
            },
        ];

        for variant in variants {
            assert_ne!(derive_sas(&variant, &responder), code);
            assert_ne!(derive_sas(&responder, &variant), code);
        }
    }

    #[test]
    fn commitment_opens_only_to_its_nonce_and_identities() {
        let initiator = PublicKey::new([1; 32]);
        let responder = PublicKey::new([2; 32]);
        let nonce = PairingNonce::new([3; 16]);
        let commitment = NonceCommitment::commit(initiator, responder, nonce);

        assert!(commitment.opens_to(initiator, responder, nonce));
        assert!(!commitment.opens_to(initiator, responder, PairingNonce::new([4; 16])));
        assert!(!commitment.opens_to(responder, initiator, nonce));
        assert!(!commitment.opens_to(initiator, PublicKey::new([9; 32]), nonce));
    }

    #[test]
    fn random_nonces_differ() {
        assert_ne!(
            PairingNonce::random().unwrap(),
            PairingNonce::random().unwrap()
        );
        assert_ne!(
            SessionNonce::random().unwrap(),
            SessionNonce::random().unwrap()
        );
    }

    #[test]
    fn pairing_nonce_debug_is_redacted() {
        assert_eq!(
            format!("{:?}", PairingNonce::new([7; 16])),
            "PairingNonce([REDACTED])"
        );
    }
}
