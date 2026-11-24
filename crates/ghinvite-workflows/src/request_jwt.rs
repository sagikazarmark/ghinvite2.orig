//! The jsonwebtoken backend for Restate request identity.
//!
//! Restate signs each request with Ed25519 (`x-restate-jwt-v1`, `alg` EdDSA),
//! and restate-sdk verifies it with jsonwebtoken 10. jsonwebtoken's
//! `rust_crypto` backend builds every algorithm it knows: RSA (`rsa` 0.9,
//! RUSTSEC-2023-0071) and P-256/P-384 (`elliptic-curve` 0.13 with
//! `crypto-bigint` 0.5, RUSTSEC-2026-0387), none of which Restate uses. So
//! the workspace enables no jsonwebtoken backend and installs this provider:
//! EdDSA verification over `ed25519-dalek`, as jsonwebtoken's own
//! `rust_crypto` EdDSA verifier does it, and nothing else. It never signs and
//! has no JWK support.
//!
//! Without an installed provider, jsonwebtoken panics on the first verified
//! request, so [`install`] runs whenever an endpoint's identity policy is
//! applied (`EndpointIdentity::apply`), before the endpoint exists.

use ed25519_dalek::{Signature, VerifyingKey};
use jsonwebtoken::crypto::{CryptoProvider, JwkUtils, JwtSigner, JwtVerifier};
use jsonwebtoken::errors::{ErrorKind, Result};
use jsonwebtoken::{Algorithm, AlgorithmFamily, DecodingKey, EncodingKey};

static PROVIDER: CryptoProvider = CryptoProvider {
    signer_factory,
    verifier_factory,
    jwk_utils: JwkUtils::new_unimplemented(),
};

/// Make [`PROVIDER`] jsonwebtoken's process-wide provider. Idempotent; an
/// already installed provider (ours, in practice: nothing else in ghinvite
/// uses jsonwebtoken 10) is kept.
pub(crate) fn install() {
    let _ = PROVIDER.install_default();
}

fn signer_factory(_: &Algorithm, _: &EncodingKey) -> Result<Box<dyn JwtSigner>> {
    Err(ErrorKind::InvalidAlgorithm.into())
}

fn verifier_factory(algorithm: &Algorithm, key: &DecodingKey) -> Result<Box<dyn JwtVerifier>> {
    if *algorithm != Algorithm::EdDSA || key.family() != AlgorithmFamily::Ed {
        return Err(ErrorKind::InvalidAlgorithm.into());
    }
    let bytes: &[u8; 32] = key
        .as_bytes()
        .try_into()
        .map_err(|_| ErrorKind::InvalidEddsaKey)?;
    let key = VerifyingKey::from_bytes(bytes).map_err(|_| ErrorKind::InvalidEddsaKey)?;
    Ok(Box::new(EdDsaVerifier(key)))
}

struct EdDsaVerifier(VerifyingKey);

impl jsonwebtoken::signature::Verifier<Vec<u8>> for EdDsaVerifier {
    fn verify(
        &self,
        message: &[u8],
        signature: &Vec<u8>,
    ) -> std::result::Result<(), jsonwebtoken::signature::Error> {
        // The non-strict check, as jsonwebtoken's own EdDSA verifier.
        ed25519_dalek::Verifier::verify(&self.0, message, &Signature::from_slice(signature)?)
    }
}

impl JwtVerifier for EdDsaVerifier {
    fn algorithm(&self) -> Algorithm {
        Algorithm::EdDSA
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public() -> [u8; 32] {
        ed25519_dalek::SigningKey::from_bytes(&[7; 32])
            .verifying_key()
            .to_bytes()
    }

    #[test]
    fn verifies_only_eddsa_with_a_32_byte_ed25519_key() {
        let public = public();
        let ed = DecodingKey::from_ed_der(&public);
        assert!(verifier_factory(&Algorithm::EdDSA, &ed).is_ok());
        for algorithm in [Algorithm::RS256, Algorithm::ES256, Algorithm::HS256] {
            assert!(verifier_factory(&algorithm, &ed).is_err());
        }
        let secret = DecodingKey::from_secret(&public);
        assert!(verifier_factory(&Algorithm::EdDSA, &secret).is_err());
        for bytes in [&public[..31], &[public.as_slice(), &[0]].concat()] {
            let short = DecodingKey::from_ed_der(bytes);
            assert!(verifier_factory(&Algorithm::EdDSA, &short).is_err());
        }
    }

    #[test]
    fn never_signs() {
        let key = EncodingKey::from_ed_der(&[0; 48]);
        assert!(signer_factory(&Algorithm::EdDSA, &key).is_err());
    }
}
