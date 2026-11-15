//! The key-wrapping handle and the wrapped-key blob:
//! `magic ‖ u32be version ‖ kid[8] ‖ AES-256-KW(DEK)`, 56 bytes.
use crate::keyring::{Keyring, Material, hex};
use crate::{Code, Error, ErrorKind, Operation, Result, labels};
use aes_kw::{KeyInit, KwAes256};
use anykms_core::{
    Capabilities, Ciphertext, Describe, KeyInfo, KeySemantics, KeyState, KeyVersion,
    KeyVersionInfo, KeyWrap, Semantics, VersionModel, VersionShape,
};
use std::{fmt, sync::Arc, sync::LazyLock};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

/// The only data-key length this KEK wraps.
pub const DEK_LEN: usize = 32;
const WRAPPED_LEN: usize = DEK_LEN + aes_kw::IV_LEN;
/// `magic ‖ version ‖ kid ‖ wrapped`.
pub const BLOB_LEN: usize = 4 + 4 + 8 + WRAPPED_LEN;

static SEMANTICS: LazyLock<KeySemantics> = LazyLock::new(|| {
    KeySemantics::new(VersionModel::Floating, VersionShape::Numeric)
        .with_ciphertext_reveals_version(true)
});

/// The blob's unauthenticated header: the selector a reader uses to pick exactly one generation.
struct Header<'a> {
    version: u32,
    kid: &'a [u8],
    wrapped: &'a [u8],
}

fn header(blob: &[u8]) -> Option<Header<'_>> {
    if blob.len() != BLOB_LEN || blob[..4] != labels::BLOB_MAGIC {
        return None;
    }
    let version = u32::from_be_bytes(blob[4..8].try_into().expect("framed"));
    (version != 0).then(|| Header {
        version,
        kid: &blob[8..16],
        wrapped: &blob[16..],
    })
}

/// The version a blob names, read without authenticating it (a selector, never a basis for
/// trust).
pub(crate) fn blob_version(blob: &[u8]) -> Option<u32> {
    header(blob).map(|h| h.version)
}

/// The keyring as an anykms key-wrapping key (`KeyWrap + Describe + Semantics`). Wraps exactly
/// 32-byte keys with AES-256-KW under the current generation's KEK; unwraps with the generation
/// the blob names. No `Rewrap`: `EnvelopeAead::rewrap` skips the key-commitment check that
/// `rewrap_local` performs, so a native one would make the weaker path the preferred one.
///
/// Every operation completes on its first poll: the KEK is in process and nothing is awaited.
#[derive(Clone)]
pub struct Kek {
    ring: Arc<Material>,
    resource: Arc<str>,
}

impl Kek {
    pub(crate) fn new(ring: &Keyring) -> Self {
        Self {
            resource: format!("{}:{}", labels::SCHEME, ring.name()).into(),
            ring: ring.0.clone(),
        }
    }

    /// The keyring this KEK wraps with.
    #[must_use]
    pub fn keyring(&self) -> Keyring {
        Keyring(self.ring.clone())
    }

    fn err(&self, kind: ErrorKind, op: Operation, msg: impl Into<String>) -> Error {
        crate::error(kind, op, msg).with_resource(&*self.resource)
    }

    fn unwrap_failed(&self, code: Option<Code>, msg: impl Into<String>) -> Error {
        let e = self.err(ErrorKind::DecryptFailed, Operation::UnwrapKey, msg);
        match code {
            Some(c) => e.with_detail(anykms_core::BackendDetail::new().with_code(c.as_str())),
            None => e,
        }
    }

    /// The blob layout, without `async`: the trait methods call this.
    pub(crate) fn wrap(&self, key: &[u8]) -> Result<Ciphertext> {
        if key.len() != DEK_LEN {
            return Err(self.err(
                ErrorKind::InvalidArgument,
                Operation::WrapKey,
                "key must be exactly 32 bytes",
            ));
        }
        let (v, g) = self.ring.current();
        let mut wrapped = [0; WRAPPED_LEN];
        KwAes256::new_from_slice(&g.kek[..])
            .expect("32-byte KEK")
            .wrap_key(key, &mut wrapped)
            .map_err(|_| self.err(ErrorKind::Backend, Operation::WrapKey, "key wrap failed"))?;
        let mut out = Vec::with_capacity(BLOB_LEN);
        out.extend_from_slice(&labels::BLOB_MAGIC);
        out.extend_from_slice(&v.to_be_bytes());
        out.extend_from_slice(&g.kid);
        out.extend_from_slice(&wrapped);
        Ok(Ciphertext::from_bytes(out))
    }

    pub(crate) fn unwrap(&self, blob: &Ciphertext) -> Result<Zeroizing<Vec<u8>>> {
        let h = header(blob.as_bytes())
            .ok_or_else(|| self.unwrap_failed(None, "not a kekring wrapped key"))?;
        let v = h.version;
        let m = &self.ring;
        let Some(g) = m.gens.get(&v) else {
            return Err(if v > m.hwm {
                self.unwrap_failed(
                    Some(Code::VersionAboveHighWaterMark),
                    format!(
                        "wrapped key names version {v}, above this keyring's high-water mark {}",
                        m.hwm
                    ),
                )
            } else {
                self.err(
                    ErrorKind::KeyVersionUnusable,
                    Operation::UnwrapKey,
                    format!("wrapped key names removed version {v}"),
                )
                .with_detail(
                    anykms_core::BackendDetail::new().with_code(Code::GenerationRemoved.as_str()),
                )
            });
        };
        if !bool::from(g.kid.ct_eq(h.kid)) {
            return Err(self.unwrap_failed(
                Some(Code::KeyIdMismatch),
                format!(
                    "wrapped key's key id {} for version {v} is not this keyring's {}",
                    hex(h.kid),
                    hex(&g.kid)
                ),
            ));
        }
        let mut dek = Zeroizing::new(vec![0; DEK_LEN]);
        KwAes256::new_from_slice(&g.kek[..])
            .expect("32-byte KEK")
            .unwrap_key(h.wrapped, &mut dek)
            .map_err(|_| self.unwrap_failed(None, "key unwrap failed"))?;
        Ok(dek)
    }

    pub(crate) fn key_info(&self) -> KeyInfo {
        let m = &self.ring;
        KeyInfo::new("AES256_KW", Capabilities::KEY_WRAP)
            .with_state(Some(KeyState::Enabled))
            .with_primary_version(Some(KeyVersion::from_u64(m.current.into())))
            .with_versions(
                m.gens
                    .keys()
                    .map(|v| {
                        KeyVersionInfo::new(KeyVersion::from_u64((*v).into()), KeyState::Enabled)
                    })
                    .collect(),
            )
    }
}

impl fmt::Debug for Kek {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Kek").field(&self.keyring()).finish()
    }
}

impl KeyWrap for Kek {
    // The portable trait is async; this local KEK completes in one poll and does no
    // work before it is polled (clippy::unused_async_trait_impl on newer toolchains).
    async fn wrap_key(&self, key: &[u8]) -> Result<Ciphertext> {
        self.wrap(key)
    }

    /// Every failure is `DecryptFailed`, except a version at or below the high-water mark that
    /// is no longer listed (`KeyVersionUnusable`, code `GenerationRemoved`). A version above the
    /// high-water mark and a key id that does not match carry the codes
    /// `VersionAboveHighWaterMark` and `KeyIdMismatch`. None carries a retry hint.
    async fn unwrap_key(&self, wrapped: &Ciphertext) -> Result<Zeroizing<Vec<u8>>> {
        self.unwrap(wrapped)
    }

    fn wrapped_key_version(&self, wrapped: &Ciphertext) -> Result<Option<KeyVersion>> {
        blob_version(wrapped.as_bytes())
            .map(|v| Some(KeyVersion::from_u64(v.into())))
            .ok_or_else(|| {
                self.err(
                    ErrorKind::InvalidArgument,
                    Operation::UnwrapKey,
                    "not a kekring wrapped key",
                )
            })
    }
}

impl Describe for Kek {
    // See `wrap_key`: one poll, no I/O.
    async fn describe(&self) -> Result<KeyInfo> {
        Ok(self.key_info())
    }
}

impl Semantics for Kek {
    fn semantics(&self) -> Option<&KeySemantics> {
        Some(&SEMANTICS)
    }
}

pub(crate) fn semantics() -> &'static KeySemantics {
    &SEMANTICS
}
