//! Production immutable keyring. Root keys are provisioned outside the application.
//! Random GCM nonces impose a 2^32 wraps/generation limit; rotate well before it.
#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]
pub mod context;
use aes_gcm::{
    Aes256Gcm, KeyInit,
    aead::{Aead, Payload},
};
use anykms_core::{
    Capabilities, Ciphertext, Describe, Error, ErrorKind, KeyInfo, KeyState, KeyVersion,
    KeyVersionInfo, KeyWrap, Operation, Result,
};
use anykms_recipes::{Envelope, EnvelopeAead, envelope::Padding};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
pub use context::{
    ContextBuilder, ContextEnvelope, EncryptionContext, EnvelopeBackend, FieldKind, FieldSpec,
    Presence, Purpose, purposes,
};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fmt, sync::Arc};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;
fn err(kind: ErrorKind, op: Operation, msg: impl Into<String>) -> Error {
    Error::new(kind, "kekring", op, msg.into())
}
fn config(msg: impl Into<String>) -> Error {
    err(ErrorKind::Configuration, Operation::Configure, msg)
}
fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}
fn version(s: &str) -> Result<u32> {
    s.parse::<u32>()
        .ok()
        .filter(|v| *v > 0 && v.to_string() == s)
        .ok_or_else(|| config("version must be canonical nonzero u32"))
}
fn hex(b: &[u8]) -> String {
    b.iter()
        .flat_map(|b| {
            let digits = b"0123456789abcdef";
            [
                char::from(digits[usize::from(b >> 4)]),
                char::from(digits[usize::from(b & 15)]),
            ]
        })
        .collect()
}
struct Generation {
    kek: Zeroizing<[u8; 32]>,
    kcv: [u8; 8],
}
struct Material {
    name: String,
    current: u32,
    gens: BTreeMap<u32, Generation>,
}
#[derive(Clone)]
pub struct Keyring(Arc<Material>);
#[derive(Clone)]
pub struct KeyringKek {
    ring: Keyring,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    pub name: String,
    pub current: u32,
    pub versions: Vec<u32>,
    pub digest: String,
}
impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} current={} versions={} fp={}",
            self.name,
            self.current,
            self.versions
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(","),
            self.digest
        )
    }
}
impl fmt::Debug for Keyring {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.fingerprint().fmt(f)
    }
}
impl fmt::Debug for KeyringKek {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.ring.fmt(f)
    }
}
fn name_len(name: &str) -> u8 {
    u8::try_from(name.len()).expect("validated keyring name is at most 64 bytes")
}
fn derive(name: &str, v: u32, root: &[u8; 32]) -> Generation {
    let hk = Hkdf::<Sha256>::new(Some(b"anykms-keyring/v1/salt"), root);
    let info = |tag: &[u8]| {
        let mut b = tag.to_vec();
        b.push(name_len(name));
        b.extend(name.as_bytes());
        b.extend(v.to_be_bytes());
        b
    };
    let mut kek = Zeroizing::new([0; 32]);
    let mut kcv = [0; 8];
    hk.expand(&info(b"anykms-keyring/v1/kek"), &mut kek[..])
        .expect("valid HKDF length");
    hk.expand(&info(b"anykms-keyring/v1/kcv"), &mut kcv)
        .expect("valid HKDF length");
    Generation { kek, kcv }
}
pub struct KeyringBuilder {
    name: String,
    current: Option<u32>,
    roots: BTreeMap<u32, Zeroizing<[u8; 32]>>,
}
impl KeyringBuilder {
    /// # Errors
    /// Returns Configuration for zero or duplicate versions, zero or duplicate roots, or more than 64 generations.
    pub fn generation(mut self, v: u32, root: [u8; 32]) -> Result<Self> {
        let root = Zeroizing::new(root);
        if v == 0 || self.roots.contains_key(&v) {
            return Err(config("duplicate or zero generation"));
        }
        if self.roots.len() >= 64 {
            return Err(config("keyring must contain 1 to 64 generations"));
        }
        if root.iter().all(|b| *b == 0)
            || self
                .roots
                .values()
                .any(|r| bool::from(r[..].ct_eq(&root[..])))
        {
            return Err(config(format!("k{v}: zero or duplicate key material")));
        }
        self.roots.insert(v, root);
        Ok(self)
    }
    /// # Errors
    /// Returns Configuration if the version is zero.
    pub fn current(mut self, v: u32) -> Result<Self> {
        if v == 0 {
            return Err(config("current must be nonzero"));
        }
        self.current = Some(v);
        Ok(self)
    }
    /// # Errors
    /// Returns Configuration for an invalid name or a current generation that is missing.
    pub fn build(self) -> Result<Keyring> {
        if !valid_name(&self.name) {
            return Err(config("name is invalid"));
        }
        let current = self.current.ok_or_else(|| config("current is missing"))?;
        if !self.roots.contains_key(&current) {
            return Err(config("current does not name a present generation"));
        }
        let gens = self
            .roots
            .iter()
            .map(|(v, r)| (*v, derive(&self.name, *v, r)))
            .collect();
        Ok(Keyring(Arc::new(Material {
            name: self.name,
            current,
            gens,
        })))
    }
}
impl Keyring {
    #[must_use]
    pub fn builder(name: &str) -> KeyringBuilder {
        KeyringBuilder {
            name: name.into(),
            current: None,
            roots: BTreeMap::new(),
        }
    }
    /// # Errors
    /// Returns Configuration for a malformed descriptor, invalid generation set, or key check mismatch. Error messages never include root material.
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.strip_suffix('\n').unwrap_or(input);
        if !input.is_ascii() || input.bytes().any(|b| b.is_ascii_whitespace()) {
            return Err(config("descriptor contains non-ASCII or whitespace"));
        }
        let mut parts = input.split(';');
        if parts.next() != Some("akr1") {
            return Err(config("descriptor prefix must be akr1"));
        }
        let mut name = None;
        let mut current = None;
        let mut roots = BTreeMap::new();
        for part in parts {
            let (key, value) = part
                .split_once('=')
                .ok_or_else(|| config("malformed descriptor field"))?;
            match key {
                "name" => {
                    if name.replace(value).is_some() {
                        return Err(config("duplicate name"));
                    }
                }
                "current" => {
                    if current.replace(version(value)?).is_some() {
                        return Err(config("duplicate current"));
                    }
                }
                _ => {
                    let v = version(
                        key.strip_prefix('k')
                            .ok_or_else(|| config("unknown descriptor field"))?,
                    )?;
                    if roots.len() >= 64 || roots.contains_key(&v) {
                        return Err(config("duplicate generation or more than 64 generations"));
                    }
                    let bad = || config(format!("k{v}: key is not 32 bytes of unpadded base64url"));
                    let (encoded, kcv) = value.split_once('.').ok_or_else(bad)?;
                    if encoded.len() != 43
                        || !encoded
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    {
                        return Err(bad());
                    }
                    let mut root = Zeroizing::new([0; 32]);
                    let size = URL_SAFE_NO_PAD
                        .decode_slice(encoded, &mut root[..])
                        .map_err(|_| bad())?;
                    if size != 32 {
                        return Err(bad());
                    }
                    if kcv.len() != 16
                        || !kcv
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    {
                        return Err(config(format!(
                            "k{v}: key check value must be 16 lowercase hex characters"
                        )));
                    }
                    roots.insert(v, (root, kcv));
                }
            }
        }
        let name = name.ok_or_else(|| config("name is missing"))?;
        if !valid_name(name) {
            return Err(config("name is invalid"));
        }
        let mut builder =
            Self::builder(name).current(current.ok_or_else(|| config("current is missing"))?)?;
        for (v, (root, kcv)) in roots {
            if !bool::from(
                hex(&derive(name, v, &root).kcv)
                    .as_bytes()
                    .ct_eq(kcv.as_bytes()),
            ) {
                return Err(config(format!("k{v}: key check value does not match")));
            }
            builder = builder.generation(v, *root)?;
        }
        builder.build()
    }
    #[must_use]
    pub fn name(&self) -> &str {
        &self.0.name
    }
    #[must_use]
    pub fn current_version(&self) -> u32 {
        self.0.current
    }
    #[must_use]
    pub fn versions(&self) -> impl ExactSizeIterator<Item = u32> + '_ {
        self.0.gens.keys().copied()
    }
    #[must_use]
    pub fn key_check_value(&self, v: u32) -> Option<[u8; 8]> {
        self.0.gens.get(&v).map(|g| g.kcv)
    }
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        let mut h = Sha256::new();
        h.update(b"anykms-keyring/v1/fp");
        h.update([name_len(self.name())]);
        h.update(self.name().as_bytes());
        h.update(self.current_version().to_be_bytes());
        for (v, g) in &self.0.gens {
            h.update(v.to_be_bytes());
            h.update(g.kcv);
        }
        Fingerprint {
            name: self.name().into(),
            current: self.current_version(),
            versions: self.versions().collect(),
            digest: hex(&h.finalize()[..8]),
        }
    }
    /// # Errors
    /// Returns `InvalidArgument` if the padding configuration is invalid.
    pub fn context_envelope(
        &self,
        padding: Padding,
    ) -> Result<ContextEnvelope<EnvelopeAead<KeyringKek>>> {
        ContextEnvelope::new(
            EnvelopeAead::new(self.name(), KeyringKek { ring: self.clone() })?
                .with_padding(padding),
            padding,
        )
        .map(|e| {
            e.with_rotation_target(
                self.name(),
                KeyVersion::new(self.current_version().to_string()),
            )
        })
    }
    /// # Errors
    /// Returns an error if the envelope or wrapped key is malformed or names a different keyring.
    pub fn needs_rewrap(&self, ct: &Ciphertext) -> Result<bool> {
        let e = Envelope::parse(ct.as_bytes())?;
        if e.kek_id() != self.name().as_bytes() {
            return Err(err(
                ErrorKind::DecryptFailed,
                Operation::Decrypt,
                "kek_id mismatch",
            ));
        }
        let v = slot_version(&e.wrapped_dek()).map_err(|_| {
            err(
                ErrorKind::DecryptFailed,
                Operation::Decrypt,
                "malformed wrapped key",
            )
        })?;
        Ok(v != self.current_version())
    }
    #[cfg(feature = "std-loaders")]
    /// # Errors
    /// Returns Configuration if the variable is absent, not UTF-8, or contains an invalid descriptor.
    pub fn from_env(var: &str) -> Result<Self> {
        let value = Zeroizing::new(
            std::env::var(var).map_err(|_| config("cannot read keyring environment variable"))?,
        );
        Self::parse(&value)
    }
    #[cfg(feature = "std-loaders")]
    /// # Errors
    /// Returns Configuration if the file cannot be read, has unsafe Unix permissions, or contains an invalid descriptor.
    pub fn from_file(path: &std::path::Path) -> Result<Self> {
        Self::from_file_with_permissions(path, false)
    }
    #[cfg(feature = "std-loaders")]
    /// # Errors
    /// Returns Configuration on file or descriptor errors, including unsafe Unix permissions unless explicitly permitted.
    pub fn from_file_with_permissions(
        path: &std::path::Path,
        allow_insecure: bool,
    ) -> Result<Self> {
        use std::io::Read;
        let mut file = std::fs::File::open(path).map_err(|_| config("cannot open keyring file"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if !allow_insecure
                && file
                    .metadata()
                    .map_err(|_| config("cannot inspect keyring file"))?
                    .permissions()
                    .mode()
                    & 0o077
                    != 0
            {
                return Err(config("keyring file is readable by group or others"));
            }
        }
        let mut value = Zeroizing::new(String::new());
        file.read_to_string(&mut value)
            .map_err(|_| config("cannot read keyring file"))?;
        Self::parse(&value)
    }
}
fn slot_version(ct: &Ciphertext) -> Result<u32> {
    let b = ct.as_bytes();
    if b.len() != 76 || &b[..4] != b"akr1" {
        return Err(err(
            ErrorKind::InvalidArgument,
            Operation::UnwrapKey,
            "malformed wrapped key",
        ));
    }
    let v = u32::from_be_bytes(b[4..8].try_into().expect("framed"));
    if v == 0 {
        return Err(err(
            ErrorKind::InvalidArgument,
            Operation::UnwrapKey,
            "malformed wrapped key",
        ));
    }
    Ok(v)
}
impl KeyringKek {
    fn aad(&self, v: u32, g: &Generation) -> Vec<u8> {
        let mut w = b"anykms-keyring/v1/wrap".to_vec();
        w.push(name_len(self.ring.name()));
        w.extend(self.ring.name().as_bytes());
        w.extend(v.to_be_bytes());
        w.extend(g.kcv);
        w
    }
}
impl KeyWrap for KeyringKek {
    // The portable trait is async; this local KEK completes in one poll and does no
    // work before it is polled (clippy::unused_async_trait_impl on newer toolchains).
    async fn wrap_key(&self, key: &[u8]) -> Result<Ciphertext> {
        if key.len() != 32 {
            return Err(err(
                ErrorKind::InvalidArgument,
                Operation::WrapKey,
                "key must be exactly 32 bytes",
            ));
        }
        let v = self.ring.current_version();
        let g = &self.ring.0.gens[&v];
        let mut nonce = [0; 12];
        getrandom::fill(&mut nonce).map_err(|_| {
            err(
                ErrorKind::Backend,
                Operation::WrapKey,
                "random source failed",
            )
        })?;
        let cipher = Aes256Gcm::new_from_slice(&g.kek[..]).expect("32-byte key");
        let n = aes_gcm::Nonce::try_from(&nonce[..]).expect("12-byte nonce");
        let ct = cipher
            .encrypt(
                &n,
                Payload {
                    msg: key,
                    aad: &self.aad(v, g),
                },
            )
            .map_err(|_| err(ErrorKind::Backend, Operation::WrapKey, "key wrap failed"))?;
        let mut out = b"akr1".to_vec();
        out.extend(v.to_be_bytes());
        out.extend(g.kcv);
        out.extend(nonce);
        out.extend(ct);
        Ok(Ciphertext::from(out))
    }
    // The portable trait is async; this local KEK completes in one poll and does no
    // work before it is polled (clippy::unused_async_trait_impl on newer toolchains).
    async fn unwrap_key(&self, ct: &Ciphertext) -> Result<Zeroizing<Vec<u8>>> {
        let v = slot_version(ct)?;
        let max = *self.ring.0.gens.keys().next_back().expect("nonempty");
        let g=self.ring.0.gens.get(&v).ok_or_else(||if v>max {err(ErrorKind::UnknownKeyVersion,Operation::UnwrapKey,format!("wrapped key names version {v}, newer than this keyring's highest version {max}")).with_retry_hint(std::time::Duration::from_secs(30))} else {err(ErrorKind::KeyVersionUnusable,Operation::UnwrapKey,format!("wrapped key names removed version {v}"))})?;
        let b = ct.as_bytes();
        if !bool::from(g.kcv.ct_eq(&b[8..16])) {
            return Err(err(
                ErrorKind::Configuration,
                Operation::UnwrapKey,
                format!("key bytes for version {v} differ from the ones that wrapped this DEK"),
            ));
        }
        let cipher = Aes256Gcm::new_from_slice(&g.kek[..]).expect("32-byte key");
        let n = aes_gcm::Nonce::try_from(&b[16..28]).expect("framed nonce");
        cipher
            .decrypt(
                &n,
                Payload {
                    msg: &b[28..],
                    aad: &self.aad(v, g),
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| {
                err(
                    ErrorKind::DecryptFailed,
                    Operation::UnwrapKey,
                    "key unwrap failed",
                )
            })
    }
    fn wrapped_key_version(&self, ct: &Ciphertext) -> Result<Option<KeyVersion>> {
        Ok(Some(KeyVersion::new(slot_version(ct)?.to_string())))
    }
}
impl Describe for KeyringKek {
    // The portable trait is async; this local KEK completes in one poll and does no
    // work before it is polled (clippy::unused_async_trait_impl on newer toolchains).
    async fn describe(&self) -> Result<KeyInfo> {
        Ok(KeyInfo::new("kekring", Capabilities::KEY_WRAP)
            .with_primary_version(Some(KeyVersion::new(
                self.ring.current_version().to_string(),
            )))
            .with_versions(
                self.ring
                    .versions()
                    .map(|v| KeyVersionInfo::new(KeyVersion::new(v.to_string()), KeyState::Enabled))
                    .collect(),
            ))
    }
}
mod private {
    pub trait Sealed {}
}
impl private::Sealed for KeyringKek {}
impl private::Sealed for &KeyringKek {}
impl private::Sealed for Arc<KeyringKek> {}
pub trait ReadyKek: KeyWrap + private::Sealed {}
impl ReadyKek for KeyringKek {}
impl ReadyKek for &KeyringKek {}
impl ReadyKek for Arc<KeyringKek> {}
fn ready<T>(future: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    let mut f = std::pin::pin!(future);
    match f
        .as_mut()
        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    {
        std::task::Poll::Ready(v) => v,
        std::task::Poll::Pending => Err(err(
            ErrorKind::Backend,
            Operation::Encrypt,
            "KEK future did not complete synchronously",
        )),
    }
}
impl<K: ReadyKek> private::Sealed for ContextEnvelope<EnvelopeAead<K>> {}
pub trait ContextEnvelopeSync: private::Sealed {
    /// # Errors
    /// Returns an error for invalid context, random-source failure, or encryption failure.
    fn encrypt_sync(&self, pt: &[u8], ctx: &EncryptionContext) -> Result<Ciphertext>;
    /// # Errors
    /// Returns an error for malformed ciphertext, a context or key mismatch, or an unavailable generation.
    fn decrypt_sync(&self, ct: &Ciphertext, ctx: &EncryptionContext) -> Result<Zeroizing<Vec<u8>>>;
    /// # Errors
    /// Returns an error for malformed ciphertext or an unavailable or mismatched wrapping key.
    fn rewrap_current_sync(&self, ct: &Ciphertext) -> Result<Ciphertext>;
}
impl<K: ReadyKek> ContextEnvelopeSync for ContextEnvelope<EnvelopeAead<K>> {
    fn encrypt_sync(&self, p: &[u8], c: &EncryptionContext) -> Result<Ciphertext> {
        ready(self.encrypt(p, c))
    }
    fn decrypt_sync(&self, p: &Ciphertext, c: &EncryptionContext) -> Result<Zeroizing<Vec<u8>>> {
        ready(self.decrypt(p, c))
    }
    fn rewrap_current_sync(&self, p: &Ciphertext) -> Result<Ciphertext> {
        ready(self.rewrap_current(p))
    }
}
#[cfg(feature = "global")]
pub mod global {
    use super::{Arc, ErrorKind, Keyring, Operation, Result, config, err};
    static RING: std::sync::OnceLock<Arc<Keyring>> = std::sync::OnceLock::new();
    /// # Errors
    /// Returns Configuration if a different descriptor is already installed.
    pub fn install(ring: Arc<Keyring>) -> Result<()> {
        let fp = ring.fingerprint();
        let current = RING.get_or_init(|| ring);
        if current.fingerprint() != fp {
            return Err(config("a different global keyring is already installed"));
        }
        Ok(())
    }
    /// # Errors
    /// Returns Unavailable until a keyring has been installed.
    pub fn get() -> Result<Arc<Keyring>> {
        RING.get().cloned().ok_or_else(|| {
            err(
                ErrorKind::Unavailable,
                Operation::Configure,
                "global keyring is not installed",
            )
        })
    }
}
impl fmt::Debug for KeyringBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyringBuilder")
            .field("name", &self.name)
            .field("current", &self.current)
            .field("versions", &self.roots.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    #[test]
    fn pending_is_backend_error_without_blocking() {
        assert_eq!(
            ready::<()>(std::future::pending()).unwrap_err().kind(),
            ErrorKind::Backend
        );
    }
}
