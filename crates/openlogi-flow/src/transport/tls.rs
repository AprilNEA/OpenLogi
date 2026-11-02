//! Ed25519 machine identities and bidirectional pinned-key TLS verification.

use std::{
    collections::BTreeSet,
    fmt,
    sync::{Arc, Mutex},
};

use rustls::{
    CertificateError, DigitallySignedStruct, DistinguishedName, Error as RustlsError,
    SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::CryptoProvider,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
};
use subtle::ConstantTimeEq;
use thiserror::Error;
use x509_parser::{oid_registry::OID_SIG_ED25519, parse_x509_certificate};

use crate::sas::{FixedBytesError, PublicKey};

/// The application-layer protocol negotiated by every Flow QUIC connection.
pub const ALPN: &[u8] = b"olf/1";

/// A persistent Ed25519 private key and its replaceable self-signed certificate.
///
/// Persist [`Self::private_key_pkcs8`] as machine-local secret state. Reloading
/// it may produce a fresh certificate while preserving the pinned public key.
#[derive(Clone)]
pub struct MachineIdentity {
    public_key: PublicKey,
    certificate: CertificateDer<'static>,
    private_key_pkcs8: Vec<u8>,
}

impl fmt::Debug for MachineIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MachineIdentity")
            .field("public_key", &self.public_key)
            .field("certificate_len", &self.certificate.len())
            .field("private_key_pkcs8", &"[REDACTED]")
            .finish()
    }
}

impl MachineIdentity {
    /// Generates a new persistent Ed25519 machine identity.
    pub fn generate() -> Result<Self, IdentityError> {
        let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519)?;
        let private_key_pkcs8 = key_pair.serialize_der();
        Self::from_key_pair(&key_pair, private_key_pkcs8)
    }

    /// Reloads a persistent identity from an Ed25519 PKCS#8 private key.
    pub fn from_pkcs8(private_key_pkcs8: Vec<u8>) -> Result<Self, IdentityError> {
        let der = PrivatePkcs8KeyDer::from(private_key_pkcs8.as_slice());
        let key_pair = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(&der, &rcgen::PKCS_ED25519)?;
        Self::from_key_pair(&key_pair, private_key_pkcs8)
    }

    /// Returns the stable raw Ed25519 public key used as Flow peer identity.
    #[must_use]
    pub const fn public_key(&self) -> PublicKey {
        self.public_key
    }

    /// Returns the current self-signed certificate in DER form.
    #[must_use]
    pub fn certificate_der(&self) -> &[u8] {
        self.certificate.as_ref()
    }

    /// Returns the secret Ed25519 PKCS#8 bytes for caller-provided persistence.
    ///
    /// Callers must protect these bytes as machine-local secret state and must
    /// never place them in OpenLogi's syncable configuration.
    #[must_use]
    pub fn private_key_pkcs8(&self) -> &[u8] {
        &self.private_key_pkcs8
    }

    pub(super) fn certificate(&self) -> CertificateDer<'static> {
        self.certificate.clone()
    }

    pub(super) fn private_key(&self) -> PrivateKeyDer<'static> {
        PrivatePkcs8KeyDer::from(self.private_key_pkcs8.clone()).into()
    }

    fn from_key_pair(
        key_pair: &rcgen::KeyPair,
        private_key_pkcs8: Vec<u8>,
    ) -> Result<Self, IdentityError> {
        let public_key = PublicKey::try_from(key_pair.public_key_raw())?;
        let parameters = rcgen::CertificateParams::new(vec!["openlogi-flow".to_owned()])?;
        let certificate = parameters.self_signed(&key_pair)?.der().clone();
        Ok(Self {
            public_key,
            certificate,
            private_key_pkcs8,
        })
    }
}

/// Failure generating, loading, or parsing an Ed25519 machine identity.
#[derive(Debug, Error)]
pub enum IdentityError {
    /// Certificate or private-key generation failed.
    #[error("identity generation failed: {0}")]
    Rcgen(#[from] rcgen::Error),
    /// An Ed25519 key did not have its required fixed width.
    #[error(transparent)]
    KeyWidth(#[from] FixedBytesError),
    /// A peer certificate was not a canonical Ed25519 leaf certificate.
    #[error("peer certificate is not a canonical Ed25519 certificate")]
    InvalidCertificate,
}

/// Whether the TLS-authenticated peer key was already trusted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionTrust {
    /// The peer key was in the configured pin set before this connection.
    Trusted,
    /// Pairing mode admitted this one unknown key, pending SAS confirmation.
    Untrusted,
}

#[derive(Debug)]
struct TrustState {
    pinned: BTreeSet<PublicKey>,
    pairing_slot: Option<Mutex<CandidateSlot>>,
}

#[derive(Debug, Default)]
struct CandidateSlot {
    holder: Option<(PublicKey, u64)>,
    next_lease: u64,
}

/// Pinned peer-key policy shared by the TLS client and server verifiers.
///
/// Pairing mode is explicit. TLS admits unknown keys as untrusted, but only
/// one connection at a time may hold the pairing slot, and only after its
/// handshake and `Hello` exchange completed ([`CandidateLease`]). The slot is
/// released when that connection is promoted after pairing or drops, so
/// neither a paired session nor a failed or abandoned dial locks pairing out.
/// Persisting a key belongs to the pairing state machine after user
/// confirmation.
#[derive(Clone, Debug)]
pub struct PeerTrust(Arc<TrustState>);

impl PeerTrust {
    /// Creates strict trust that accepts only the supplied peer keys.
    #[must_use]
    pub fn pinned(keys: impl IntoIterator<Item = PublicKey>) -> Self {
        Self(Arc::new(TrustState {
            pinned: keys.into_iter().collect(),
            pairing_slot: None,
        }))
    }

    /// Creates explicit pairing mode, while continuing to trust existing pins.
    #[must_use]
    pub fn pairing(keys: impl IntoIterator<Item = PublicKey>) -> Self {
        Self(Arc::new(TrustState {
            pinned: keys.into_iter().collect(),
            pairing_slot: Some(Mutex::new(CandidateSlot::default())),
        }))
    }

    /// Returns the unknown key whose live connection holds the pairing slot.
    #[must_use]
    pub fn pairing_candidate(&self) -> Option<PublicKey> {
        self.0
            .pairing_slot
            .as_ref()
            .and_then(|slot| slot.lock().ok().and_then(|slot| slot.holder))
            .map(|(key, _)| key)
    }

    /// Classifies a TLS identity without reserving anything.
    ///
    /// While another key holds the pairing slot, further unknown keys are
    /// refused early; [`Self::lease_candidate`] remains the authority.
    pub(super) fn classify(&self, key: PublicKey) -> Result<SessionTrust, RustlsError> {
        if self
            .0
            .pinned
            .iter()
            .any(|pinned| constant_time_key_eq(*pinned, key))
        {
            return Ok(SessionTrust::Trusted);
        }
        let Some(slot) = &self.0.pairing_slot else {
            return Err(untrusted_certificate());
        };
        let slot = slot
            .lock()
            .map_err(|_| RustlsError::General("pairing candidate lock poisoned".to_owned()))?;
        match slot.holder {
            Some((holder, _)) if !constant_time_key_eq(holder, key) => Err(untrusted_certificate()),
            _ => Ok(SessionTrust::Untrusted),
        }
    }

    /// Claims the pairing slot for one completed untrusted connection.
    ///
    /// Returns `None` outside pairing mode or while any live connection,
    /// including another one from the same key, holds the slot.
    pub(super) fn lease_candidate(&self, key: PublicKey) -> Option<CandidateLease> {
        let mut slot = self.0.pairing_slot.as_ref()?.lock().ok()?;
        if slot.holder.is_some() {
            return None;
        }
        let id = slot.next_lease;
        slot.next_lease = slot.next_lease.wrapping_add(1);
        slot.holder = Some((key, id));
        Some(CandidateLease {
            trust: self.clone(),
            key,
            id,
        })
    }
}

/// Exclusive hold on the pairing slot, released when dropped.
///
/// Each lease carries a unique id, so a stale lease can never clear a newer
/// holder.
#[derive(Debug)]
pub struct CandidateLease {
    trust: PeerTrust,
    key: PublicKey,
    id: u64,
}

impl CandidateLease {
    /// Returns the unknown key holding the slot.
    #[must_use]
    pub const fn key(&self) -> PublicKey {
        self.key
    }
}

impl Drop for CandidateLease {
    fn drop(&mut self) {
        let Some(slot) = &self.trust.0.pairing_slot else {
            return;
        };
        let mut slot = match slot.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        if slot.holder == Some((self.key, self.id)) {
            slot.holder = None;
        }
    }
}

/// Extracts the raw Ed25519 identity from a DER certificate's SPKI.
pub fn public_key_from_certificate(certificate: &[u8]) -> Result<PublicKey, IdentityError> {
    let (remaining, certificate) =
        parse_x509_certificate(certificate).map_err(|_| IdentityError::InvalidCertificate)?;
    if !remaining.is_empty() {
        return Err(IdentityError::InvalidCertificate);
    }
    let spki = certificate.public_key();
    if spki.algorithm.algorithm != OID_SIG_ED25519
        || spki.algorithm.parameters.is_some()
        || spki.subject_public_key.unused_bits != 0
    {
        return Err(IdentityError::InvalidCertificate);
    }
    PublicKey::try_from(spki.subject_public_key.data.as_ref()).map_err(IdentityError::from)
}

fn verify_peer(
    policy: &PeerTrust,
    end_entity: &CertificateDer<'_>,
    intermediates: &[CertificateDer<'_>],
) -> Result<(), RustlsError> {
    if !intermediates.is_empty() {
        return Err(RustlsError::InvalidCertificate(
            CertificateError::BadEncoding,
        ));
    }
    let key = public_key_from_certificate(end_entity.as_ref())
        .map_err(|_| RustlsError::InvalidCertificate(CertificateError::BadEncoding))?;
    policy.classify(key).map(|_| ())
}

fn constant_time_key_eq(first: PublicKey, second: PublicKey) -> bool {
    bool::from(first.as_bytes().ct_eq(second.as_bytes()))
}

fn untrusted_certificate() -> RustlsError {
    RustlsError::InvalidCertificate(CertificateError::UnknownIssuer)
}

#[derive(Debug)]
pub(super) struct PinnedServerVerifier {
    policy: PeerTrust,
    provider: Arc<CryptoProvider>,
}

impl PinnedServerVerifier {
    pub(super) fn new(policy: PeerTrust, provider: Arc<CryptoProvider>) -> Self {
        Self { policy, provider }
    }
}

impl ServerCertVerifier for PinnedServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        verify_peer(&self.policy, end_entity, intermediates)?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            certificate,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            certificate,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[derive(Debug)]
pub(super) struct PinnedClientVerifier {
    policy: PeerTrust,
    provider: Arc<CryptoProvider>,
}

impl PinnedClientVerifier {
    pub(super) fn new(policy: PeerTrust, provider: Arc<CryptoProvider>) -> Self {
        Self { policy, provider }
    }
}

impl ClientCertVerifier for PinnedClientVerifier {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, RustlsError> {
        verify_peer(&self.policy, end_entity, intermediates)?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            certificate,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            certificate,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_identity_round_trips_through_pkcs8_and_certificate() {
        let identity = MachineIdentity::generate().unwrap();
        let reloaded = MachineIdentity::from_pkcs8(identity.private_key_pkcs8().to_vec()).unwrap();
        assert_eq!(identity.public_key(), reloaded.public_key());
        assert_eq!(
            public_key_from_certificate(reloaded.certificate_der()).unwrap(),
            identity.public_key()
        );
    }

    #[test]
    fn classification_alone_never_claims_the_pairing_slot() {
        let policy = PeerTrust::pairing([]);
        for key in [PublicKey::new([1; 32]), PublicKey::new([2; 32])] {
            assert_eq!(policy.classify(key).unwrap(), SessionTrust::Untrusted);
        }
        assert_eq!(policy.pairing_candidate(), None);
    }

    #[test]
    fn one_lease_holds_the_slot_until_dropped() {
        let policy = PeerTrust::pairing([]);
        let (first, second) = (PublicKey::new([1; 32]), PublicKey::new([2; 32]));

        let lease = policy.lease_candidate(first).unwrap();
        assert_eq!(lease.key(), first);
        assert_eq!(policy.pairing_candidate(), Some(first));
        policy.classify(second).unwrap_err();
        assert!(policy.lease_candidate(second).is_none());
        assert!(
            policy.lease_candidate(first).is_none(),
            "a second connection from the holder must not share the slot"
        );

        drop(lease);
        assert_eq!(policy.pairing_candidate(), None);
        let _lease = policy.lease_candidate(second).unwrap();
        assert_eq!(policy.pairing_candidate(), Some(second));
    }

    #[test]
    fn a_stale_lease_never_clears_a_newer_holder() {
        let policy = PeerTrust::pairing([]);
        let key = PublicKey::new([1; 32]);
        let first = policy.lease_candidate(key).unwrap();
        let stale_id = first.id;
        drop(first);
        let _current = policy.lease_candidate(key).unwrap();
        // A late destructor for the earlier lease of the same key.
        drop(CandidateLease {
            trust: policy.clone(),
            key,
            id: stale_id,
        });
        assert_eq!(policy.pairing_candidate(), Some(key));
    }

    #[test]
    fn pinned_mode_never_leases() {
        let policy = PeerTrust::pinned([]);
        assert!(policy.lease_candidate(PublicKey::new([1; 32])).is_none());
        policy.classify(PublicKey::new([1; 32])).unwrap_err();
    }

    #[test]
    fn existing_pin_stays_trusted_in_pairing_mode() {
        let pinned = PublicKey::new([1; 32]);
        let policy = PeerTrust::pairing([pinned]);
        assert_eq!(policy.classify(pinned).unwrap(), SessionTrust::Trusted);
        assert!(policy.pairing_candidate().is_none());
    }
}
