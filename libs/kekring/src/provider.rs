//! An anykms [`Provider`] over a fixed set of keyrings, addressed as `kekring:<name>`.
use crate::kek::{Kek, semantics};
use crate::keyring::valid_name;
use crate::{Error, ErrorKind, Keyring, Operation, Result, labels};
use anykms_core::{
    Ciphertext, Describe, DescribeKey, IntoKeyUri, KeyInfo, KeySemantics, KeyUri, KeyVersion,
    KeyWrap, KeyWrapKey, OpenOptions, Provider, Semantics,
};
use std::{collections::BTreeMap, fmt, sync::Arc};
use zeroize::Zeroizing;

/// Opens the keyrings it was built with as key-wrapping keys. The URI grammar is
/// `kekring:<name>` (the keyring name, nothing else); anything else is `Configuration` with
/// `Operation::ParseUri`. Opening does no I/O: a name the provider does not hold opens, and
/// every operation on it fails with `NotFound`, as on every anykms backend.
///
/// Register it with `Registry::builder().provider(..)`; anykms's default providers do not
/// include it.
#[derive(Clone, Default)]
pub struct KekringProvider {
    rings: BTreeMap<String, Kek>,
}

impl KekringProvider {
    /// # Errors
    /// `Configuration` when two keyrings share a name.
    pub fn new(rings: impl IntoIterator<Item = Keyring>) -> Result<Self> {
        let mut map = BTreeMap::new();
        for ring in rings {
            let name = ring.name().to_owned();
            if map.insert(name, ring.kek()).is_some() {
                return Err(crate::error(
                    ErrorKind::Configuration,
                    Operation::Configure,
                    "two keyrings share a name",
                ));
            }
        }
        Ok(Self { rings: map })
    }

    /// The typed constructor behind [`Provider::key_wrap`].
    ///
    /// # Errors
    /// `Configuration` (`Operation::ParseUri`) for a URI outside the grammar.
    pub fn key_wrap_key(&self, uri: impl IntoKeyUri) -> Result<ProviderKey> {
        let uri = uri.into_key_uri()?;
        let name = Some(uri.rest())
            .filter(|n| uri.scheme() == labels::SCHEME && valid_name(n))
            .ok_or_else(|| {
                crate::error(
                    ErrorKind::Configuration,
                    Operation::ParseUri,
                    "expected kekring:<name>, the keyring name made of [a-z0-9_-] (at most 64 bytes)",
                )
                .with_resource(uri.without_query())
            })?;
        Ok(ProviderKey {
            uri: uri.without_query().into(),
            kek: self.rings.get(name).cloned(),
        })
    }
}

impl fmt::Debug for KekringProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KekringProvider")
            .field("keyrings", &self.rings.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Provider for KekringProvider {
    fn schemes(&self) -> &[&'static str] {
        &[labels::SCHEME]
    }

    fn key_wrap(&self, uri: &KeyUri, _: &OpenOptions) -> Result<KeyWrapKey> {
        let k = Arc::new(self.key_wrap_key(uri)?);
        let mut key = KeyWrapKey::new(labels::SCHEME, k.clone())
            .with_describe(k.clone())
            .with_semantics(semantics().clone());
        if let Some(kek) = &k.kek {
            key = key.with_concrete(Arc::new(kek.clone()));
        }
        Ok(key)
    }

    fn describe(&self, uri: &KeyUri, _: &OpenOptions) -> Result<DescribeKey> {
        let k = Arc::new(self.key_wrap_key(uri)?);
        Ok(DescribeKey::new(labels::SCHEME, k).with_semantics(semantics().clone()))
    }
}

/// What [`KekringProvider`] opens: the named keyring's [`Kek`], or a handle whose every
/// operation fails with `NotFound` when the provider holds no keyring of that name.
#[derive(Clone)]
pub struct ProviderKey {
    uri: Arc<str>,
    kek: Option<Kek>,
}

impl ProviderKey {
    /// The KEK, if the provider holds the keyring.
    #[must_use]
    pub fn kek(&self) -> Option<&Kek> {
        self.kek.as_ref()
    }

    fn resolve(&self, op: Operation) -> Result<&Kek> {
        self.kek.as_ref().ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                labels::SCHEME,
                op,
                "keyring not provided",
            )
            .with_resource(&*self.uri)
        })
    }
}

impl fmt::Debug for ProviderKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderKey")
            .field("uri", &self.uri)
            .field("kek", &self.kek)
            .finish()
    }
}

impl KeyWrap for ProviderKey {
    // Forwards to `Kek`: one poll, no I/O.
    async fn wrap_key(&self, key: &[u8]) -> Result<Ciphertext> {
        self.resolve(Operation::WrapKey)?.wrap(key)
    }

    async fn unwrap_key(&self, wrapped: &Ciphertext) -> Result<Zeroizing<Vec<u8>>> {
        self.resolve(Operation::UnwrapKey)?.unwrap(wrapped)
    }

    fn wrapped_key_version(&self, wrapped: &Ciphertext) -> Result<Option<KeyVersion>> {
        self.resolve(Operation::UnwrapKey)?
            .wrapped_key_version(wrapped)
    }
}

impl Describe for ProviderKey {
    async fn describe(&self) -> Result<KeyInfo> {
        Ok(self.resolve(Operation::Describe)?.key_info())
    }
}

impl Semantics for ProviderKey {
    fn semantics(&self) -> Option<&KeySemantics> {
        Some(semantics())
    }
}
