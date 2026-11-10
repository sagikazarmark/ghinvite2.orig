//! Mandatory typed, canonical record binding for envelope encryption.
use anykms_core::{Aead, Ciphertext, Error, ErrorKind, KeyVersion, KeyWrap, Operation, Result};
pub use anykms_recipes::envelope::Padding;
use anykms_recipes::{EnvelopeAead, EnvelopeKeyring};
use std::{collections::BTreeMap, marker::PhantomData};
use zeroize::Zeroizing;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Str,
    Decimal,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    Required,
    Optional,
}
#[derive(Clone, Copy, Debug)]
pub struct FieldSpec {
    pub key: &'static str,
    pub kind: FieldKind,
    pub presence: Presence,
}
pub trait Purpose {
    const NAME: &'static str;
    const FIELDS: &'static [FieldSpec];
    const PADDING: Padding;
}
fn invalid(message: &'static str) -> Error {
    Error::new(
        ErrorKind::InvalidArgument,
        "context",
        Operation::Configure,
        message,
    )
}
fn identifier(s: &str, max: usize, separator: u8) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.as_bytes()[0].is_ascii_lowercase()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == separator)
}
#[derive(Clone)]
pub struct EncryptionContext {
    fields: BTreeMap<String, String>,
    padding: Padding,
    aad: Vec<u8>,
}
impl EncryptionContext {
    #[must_use]
    pub fn builder<P: Purpose>() -> ContextBuilder<P> {
        ContextBuilder {
            fields: vec![],
            marker: PhantomData,
        }
    }
    #[must_use]
    pub fn purpose(&self) -> &str {
        &self.fields["purpose"]
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }
    #[must_use]
    pub fn padding(&self) -> Padding {
        self.padding
    }
    #[must_use]
    pub fn as_aad(&self) -> &[u8] {
        &self.aad
    }
}
pub struct ContextBuilder<P> {
    fields: Vec<(&'static str, String, FieldKind)>,
    marker: PhantomData<P>,
}
impl<P: Purpose> ContextBuilder<P> {
    #[must_use]
    pub fn str(mut self, key: &'static str, value: impl AsRef<str>) -> Self {
        self.fields
            .push((key, value.as_ref().into(), FieldKind::Str));
        self
    }
    #[must_use]
    pub fn u64(mut self, key: &'static str, value: u64) -> Self {
        self.fields
            .push((key, value.to_string(), FieldKind::Decimal));
        self
    }
    ///
    /// # Errors
    /// Returns `InvalidArgument` for invalid declarations, undeclared/duplicate/missing fields, kind mismatches, noncanonical numbers or value-length violations.
    pub fn build(self) -> Result<EncryptionContext> {
        if !identifier(P::NAME, 48, b'-') || P::FIELDS.len() > 15 {
            return Err(invalid("invalid purpose declaration"));
        }
        let mut specs = BTreeMap::new();
        for s in P::FIELDS {
            if !identifier(s.key, 32, b'_')
                || s.key == "purpose"
                || specs.insert(s.key, s).is_some()
            {
                return Err(invalid("invalid purpose field declaration"));
            }
        }
        let mut fields: BTreeMap<String, String> = BTreeMap::new();
        fields.insert("purpose".into(), P::NAME.into());
        for (key, value, kind) in self.fields {
            let spec = specs
                .get(key)
                .ok_or_else(|| invalid("undeclared context field"))?;
            if value.is_empty() || value.len() > 255 {
                return Err(invalid("context value must contain 1 to 255 UTF-8 bytes"));
            }
            if spec.kind == FieldKind::Str && kind != FieldKind::Str {
                return Err(invalid("context field kind mismatch"));
            }
            if spec.kind == FieldKind::Decimal
                && value
                    .parse::<u64>()
                    .ok()
                    .is_none_or(|n| n.to_string() != value)
            {
                return Err(invalid("context decimal is not canonical u64"));
            }
            if fields.insert(key.into(), value).is_some() {
                return Err(invalid("duplicate context field"));
            }
        }
        for spec in P::FIELDS {
            if spec.presence == Presence::Required && !fields.contains_key(spec.key) {
                return Err(invalid("missing required context field"));
            }
        }
        let mut aad = b"anykms-ctx/v1\0".to_vec();
        aad.push(u8::try_from(fields.len()).map_err(|_| invalid("too many context fields"))?);
        for (k, v) in &fields {
            aad.push(u8::try_from(k.len()).map_err(|_| invalid("context key too long"))?);
            aad.extend(k.as_bytes());
            aad.extend(
                u16::try_from(v.len())
                    .map_err(|_| invalid("context value too long"))?
                    .to_be_bytes(),
            );
            aad.extend(v.as_bytes());
        }
        Ok(EncryptionContext {
            fields,
            padding: P::PADDING,
            aad,
        })
    }
}
mod private {
    pub trait Sealed {}
}
/// Sealed implementation detail; applications cannot create another backend.
#[allow(async_fn_in_trait)]
pub trait EnvelopeBackend: private::Sealed + Send + Sync {
    #[doc(hidden)]
    fn backend_padding(&self) -> Padding;
    #[doc(hidden)]
    fn encrypt_context(
        &self,
        pt: &[u8],
        ctx: &EncryptionContext,
    ) -> impl std::future::Future<Output = Result<Ciphertext>> + Send;
    #[doc(hidden)]
    fn decrypt_context(
        &self,
        ct: &Ciphertext,
        ctx: &EncryptionContext,
    ) -> impl std::future::Future<Output = Result<Zeroizing<Vec<u8>>>> + Send;
    #[doc(hidden)]
    fn rewrap_context(
        &self,
        ct: &Ciphertext,
    ) -> impl std::future::Future<Output = Result<Ciphertext>> + Send;
    #[doc(hidden)]
    fn verify_context(
        &self,
        ct: &Ciphertext,
        ctx: &EncryptionContext,
    ) -> impl std::future::Future<Output = Result<Ciphertext>> + Send;
    #[doc(hidden)]
    fn context_version(&self, ct: &Ciphertext) -> Result<Option<KeyVersion>>;
}
macro_rules! backend { ($ty:ident $(,$bound:ident)?) => {
 impl<K:KeyWrap $(+ $bound)?> private::Sealed for $ty<K> {}
 impl<K:KeyWrap $(+ $bound)?> EnvelopeBackend for $ty<K> {
 fn backend_padding(&self)->Padding { self.padding() }
 async fn encrypt_context(&self,p:&[u8],c:&EncryptionContext)->Result<Ciphertext> { self.encrypt(p,c.as_aad()).await }
 async fn decrypt_context(&self,p:&Ciphertext,c:&EncryptionContext)->Result<Zeroizing<Vec<u8>>> { self.decrypt(p,c.as_aad()).await }
 async fn rewrap_context(&self,p:&Ciphertext)->Result<Ciphertext> { self.rewrap_local(p).await }
 async fn verify_context(&self,p:&Ciphertext,c:&EncryptionContext)->Result<Ciphertext> { self.rewrap_verified(p,c.as_aad()).await }
 fn context_version(&self,p:&Ciphertext)->Result<Option<KeyVersion>> { self.ciphertext_version(p) }
 }
}; }
backend!(EnvelopeAead);
backend!(EnvelopeKeyring, Clone);
#[derive(Debug)]
pub struct ContextEnvelope<A: EnvelopeBackend> {
    inner: A,
    padding: Padding,
    rotation: Option<(Vec<u8>, KeyVersion)>,
}
impl<A: EnvelopeBackend> ContextEnvelope<A> {
    ///
    /// # Errors
    /// Returns `InvalidArgument` when the backend and requested padding policies differ.
    pub fn new(inner: A, padding: Padding) -> Result<Self> {
        if inner.backend_padding() != padding {
            return Err(invalid("envelope padding does not match"));
        }
        Ok(Self {
            inner,
            padding,
            rotation: None,
        })
    }
    /// Supplies nonsecret rotation selection metadata. It never influences encryption or decryption.
    #[must_use]
    pub fn with_rotation_target(mut self, kek_id: impl AsRef<[u8]>, version: KeyVersion) -> Self {
        self.rotation = Some((kek_id.as_ref().to_vec(), version));
        self
    }
    /// Selects rows for rewrap without authenticating them; rewrap itself checks commitment.
    ///
    /// # Errors
    /// Returns `Unsupported` without rotation metadata, or the envelope/backend version parse error.
    pub fn needs_rewrap(&self, ct: &Ciphertext) -> Result<bool> {
        let (id, current) = self.rotation.as_ref().ok_or_else(|| {
            Error::new(
                ErrorKind::Unsupported,
                "context",
                Operation::Rewrap,
                "rotation metadata unavailable",
            )
        })?;
        let env = crate::Envelope::parse(ct.as_bytes())?;
        if env.kek_id() != id {
            return Err(Error::new(
                ErrorKind::DecryptFailed,
                "context",
                Operation::Rewrap,
                "kek_id mismatch",
            ));
        }
        let v = self.ciphertext_version(ct).map_err(|_| {
            Error::new(
                ErrorKind::DecryptFailed,
                "context",
                Operation::Rewrap,
                "malformed wrapped key",
            )
        })?;
        Ok(v.as_ref() != Some(current))
    }
    pub fn padding(&self) -> Padding {
        self.padding
    }
    ///
    /// # Errors
    /// Returns a bounded context/format error, or propagates the underlying key/envelope error; no context values appear in the error.
    pub async fn encrypt(&self, p: &[u8], c: &EncryptionContext) -> Result<Ciphertext> {
        if c.padding() != self.padding {
            return Err(invalid("purpose padding does not match this envelope"));
        }
        self.inner.encrypt_context(p, c).await
    }
    ///
    /// # Errors
    /// Returns a bounded context/format error, or propagates the underlying key/envelope error; no context values appear in the error.
    pub async fn decrypt(
        &self,
        p: &Ciphertext,
        c: &EncryptionContext,
    ) -> Result<Zeroizing<Vec<u8>>> {
        self.inner.decrypt_context(p, c).await
    }
    ///
    /// # Errors
    /// Returns a bounded context/format error, or propagates the underlying key/envelope error; no context values appear in the error.
    pub async fn rewrap_current(&self, p: &Ciphertext) -> Result<Ciphertext> {
        self.inner.rewrap_context(p).await
    }
    ///
    /// # Errors
    /// Returns a bounded context/format error, or propagates the underlying key/envelope error; no context values appear in the error.
    pub async fn rewrap_verified(
        &self,
        p: &Ciphertext,
        c: &EncryptionContext,
    ) -> Result<Ciphertext> {
        self.inner.verify_context(p, c).await
    }
    ///
    /// # Errors
    /// Returns a bounded context/format error, or propagates the underlying key/envelope error; no context values appear in the error.
    pub fn ciphertext_version(&self, p: &Ciphertext) -> Result<Option<KeyVersion>> {
        self.inner.context_version(p)
    }
}
/// Types accepted by `purpose!`; also lets macro expansion preserve optional presence.
#[doc(hidden)]
pub trait ContextValue {
    const KIND: FieldKind;
    const PRESENCE: Presence;
    fn insert<P: Purpose>(&self, b: ContextBuilder<P>, key: &'static str) -> ContextBuilder<P>;
}
impl ContextValue for str {
    const KIND: FieldKind = FieldKind::Str;
    const PRESENCE: Presence = Presence::Required;
    fn insert<P: Purpose>(&self, b: ContextBuilder<P>, k: &'static str) -> ContextBuilder<P> {
        b.str(k, self)
    }
}
impl ContextValue for &str {
    const KIND: FieldKind = FieldKind::Str;
    const PRESENCE: Presence = Presence::Required;
    fn insert<P: Purpose>(&self, b: ContextBuilder<P>, k: &'static str) -> ContextBuilder<P> {
        b.str(k, self)
    }
}
impl ContextValue for String {
    const KIND: FieldKind = FieldKind::Str;
    const PRESENCE: Presence = Presence::Required;
    fn insert<P: Purpose>(&self, b: ContextBuilder<P>, k: &'static str) -> ContextBuilder<P> {
        b.str(k, self)
    }
}
impl ContextValue for u64 {
    const KIND: FieldKind = FieldKind::Decimal;
    const PRESENCE: Presence = Presence::Required;
    fn insert<P: Purpose>(&self, b: ContextBuilder<P>, k: &'static str) -> ContextBuilder<P> {
        b.u64(k, *self)
    }
}
impl<T: ContextValue> ContextValue for Option<T> {
    const KIND: FieldKind = T::KIND;
    const PRESENCE: Presence = Presence::Optional;
    fn insert<P: Purpose>(&self, b: ContextBuilder<P>, k: &'static str) -> ContextBuilder<P> {
        match self {
            Some(v) => v.insert(b, k),
            None => b,
        }
    }
}
#[doc(hidden)]
pub use anykms_core::Error as ContextError;
#[macro_export]
macro_rules! purpose {
 (@key r#type) => { "type" };
 (@key $field:ident) => { stringify!($field) };
 ($vis:vis struct $name:ident $(<$life:lifetime>)? = $purpose:literal padding = block($n:literal) { $($field:ident:$ty:ty),* $(,)? }) => { $crate::purpose!(@impl $vis struct $name $(<$life>)? = $purpose, $crate::context::Padding::block($n); {$($field:$ty),*}); };
 ($vis:vis struct $name:ident $(<$life:lifetime>)? = $purpose:literal padding = none { $($field:ident:$ty:ty),* $(,)? }) => { $crate::purpose!(@impl $vis struct $name $(<$life>)? = $purpose, $crate::context::Padding::NONE; {$($field:$ty),*}); };
 ($vis:vis struct $name:ident $(<$life:lifetime>)? = $purpose:literal { $($field:ident:$ty:ty),* $(,)? }) => { $crate::purpose!(@impl $vis struct $name $(<$life>)? = $purpose, $crate::context::Padding::NONE; {$($field:$ty),*}); };
 (@impl $vis:vis struct $name:ident $(<$life:lifetime>)? = $purpose:literal, $padding:expr; {$($field:ident:$ty:ty),*}) => {
  $vis struct $name $(<$life>)? { $($vis $field:$ty),* }
 impl $(<$life>)? $crate::context::Purpose for $name $(<$life>)? { const NAME:&'static str=$purpose; const PADDING:$crate::context::Padding=$padding; const FIELDS:&'static [$crate::context::FieldSpec]=&[$($crate::context::FieldSpec{key:$crate::purpose!(@key $field),kind:<$ty as $crate::context::ContextValue>::KIND,presence:<$ty as $crate::context::ContextValue>::PRESENCE}),*]; }
 const _:()=assert!(<[&str]>::len(&[$(stringify!($field)),*])<=15);
 impl $(<$life>)? std::fmt::Debug for $name $(<$life>)? { fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result { f.debug_struct(stringify!($name)).field("purpose", &$purpose).finish_non_exhaustive() } }
 impl $(<$life>)? TryFrom<&$name $(<$life>)?> for $crate::context::EncryptionContext { type Error=$crate::context::ContextError; fn try_from(v:&$name $(<$life>)?)->Result<Self,Self::Error> { let b=Self::builder::<$name $(<$life>)?>(); $(let b=$crate::context::ContextValue::insert(&v.$field,b,$crate::purpose!(@key $field));)* b.build() } }
 };
}
pub mod purposes {
    crate::purpose! { pub struct UserToken<'a> = "user-token" padding = block(64) { app_client_id:&'a str,user_id:u64 } }
    crate::purpose! { pub struct InstallationTokenCache<'a> = "installation-token-cache" padding = block(64) { app_id:u64,installation_id:u64,scope:&'a str,expires_at:u64 } }
    crate::purpose! { pub struct Session<'a> = "session" { session_id:&'a str } }
    crate::purpose! { pub struct BulkEmailReview<'a> = "bulk-email-review" padding = block(256) { email_ref:&'a str,batch_id:&'a str,row:u64,account_id:u64,installation_id:u64 } }
    crate::purpose! { pub struct BulkEmail<'a> = "bulk-email" padding = block(256) { email_ref:&'a str,batch_id:&'a str,row:u64,account_id:u64,installation_id:u64 } }
    crate::purpose! { pub struct PendingSignIn<'a> = "pending-signin" { session_id:&'a str } }
    crate::purpose! { pub struct AttemptContinuation<'a> = "attempt-continuation" { scope:&'a str,binding:&'a str } }
    crate::purpose! { pub struct RestateSealed<'a> = "restate-sealed" { r#type: &'a str } }
}

impl std::fmt::Debug for EncryptionContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptionContext")
            .field("purpose", &self.purpose())
            .field("keys", &self.fields.keys().collect::<Vec<_>>())
            .field("padding", &self.padding)
            .finish_non_exhaustive()
    }
}
impl<P> std::fmt::Debug for ContextBuilder<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextBuilder")
            .field(
                "keys",
                &self.fields.iter().map(|(k, _, _)| k).collect::<Vec<_>>(),
            )
            .finish()
    }
}
