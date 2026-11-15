//! The anykms canonical AAD, version 1 (`anykms/docs/aad-format.md`), and typed purposes over it.
//!
//! anykms specifies the bytes and ships no type that produces them; this module is one
//! implementation, checked against upstream's `aad_v1.json` vectors. [`Aad::encode`] is the
//! specification's encoder over runtime strings. [`Aad::builder`] is the typed path: a
//! [`Purpose`] declares its name and its fields once, the declaration is checked at compile time,
//! and every builder for it must supply exactly the declared fields.
use crate::{Error, ErrorKind, Operation, Padding, Result};
use std::fmt;

/// The label that opens every v1 encoding.
pub const LABEL: &[u8; 14] = b"anykms/aad/v1\0";
const MAX_PURPOSE_LEN: usize = 48;
const MAX_KEY_LEN: usize = 32;
const MAX_VALUE_LEN: usize = 255;
/// Application fields; `purpose` makes 16.
const MAX_FIELDS: usize = 15;
const MAX_TOTAL_LEN: usize = 3072;
const PURPOSE_KEY: &str = "purpose";

/// The rule an input breaks. [`Rule::as_str`] is the name upstream's invalid vectors use; errors
/// carry it as `aad:<name>` in [`BackendDetail::code`](anykms_core::BackendDetail::code).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    EmptyPurpose,
    PurposeTooLong,
    PurposeCharset,
    ReservedKey,
    DuplicateKey,
    EmptyKey,
    KeyTooLong,
    KeyCharset,
    EmptyValue,
    ValueTooLong,
    ValueControlChar,
    TooManyFields,
    TotalTooLong,
    /// kekring's typed path only: a field the purpose does not declare.
    UndeclaredField,
    /// kekring's typed path only: a declared field that was not supplied.
    MissingField,
}

impl Rule {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EmptyPurpose => "empty_purpose",
            Self::PurposeTooLong => "purpose_too_long",
            Self::PurposeCharset => "purpose_charset",
            Self::ReservedKey => "reserved_key",
            Self::DuplicateKey => "duplicate_key",
            Self::EmptyKey => "empty_key",
            Self::KeyTooLong => "key_too_long",
            Self::KeyCharset => "key_charset",
            Self::EmptyValue => "empty_value",
            Self::ValueTooLong => "value_too_long",
            Self::ValueControlChar => "value_control_char",
            Self::TooManyFields => "too_many_fields",
            Self::TotalTooLong => "total_too_long",
            Self::UndeclaredField => "undeclared_field",
            Self::MissingField => "missing_field",
        }
    }

    const fn code(self) -> &'static str {
        match self {
            Self::EmptyPurpose => "aad:empty_purpose",
            Self::PurposeTooLong => "aad:purpose_too_long",
            Self::PurposeCharset => "aad:purpose_charset",
            Self::ReservedKey => "aad:reserved_key",
            Self::DuplicateKey => "aad:duplicate_key",
            Self::EmptyKey => "aad:empty_key",
            Self::KeyTooLong => "aad:key_too_long",
            Self::KeyCharset => "aad:key_charset",
            Self::EmptyValue => "aad:empty_value",
            Self::ValueTooLong => "aad:value_too_long",
            Self::ValueControlChar => "aad:value_control_char",
            Self::TooManyFields => "aad:too_many_fields",
            Self::TotalTooLong => "aad:total_too_long",
            Self::UndeclaredField => "aad:undeclared_field",
            Self::MissingField => "aad:missing_field",
        }
    }

    /// The rule of an error this module returned.
    #[must_use]
    pub fn of(error: &Error) -> Option<Self> {
        const ALL: [Rule; 15] = [
            Rule::EmptyPurpose,
            Rule::PurposeTooLong,
            Rule::PurposeCharset,
            Rule::ReservedKey,
            Rule::DuplicateKey,
            Rule::EmptyKey,
            Rule::KeyTooLong,
            Rule::KeyCharset,
            Rule::EmptyValue,
            Rule::ValueTooLong,
            Rule::ValueControlChar,
            Rule::TooManyFields,
            Rule::TotalTooLong,
            Rule::UndeclaredField,
            Rule::MissingField,
        ];
        let code = error.detail()?.code()?;
        ALL.into_iter().find(|r| r.code() == code)
    }
}

/// Never names a value: at most the rule and the key.
fn violation(rule: Rule, key: Option<&str>) -> Error {
    let message = match key {
        Some(k) if is_name(k.as_bytes(), b'_') && k.len() <= MAX_KEY_LEN => {
            format!("canonical AAD rule {} (key {k})", rule.as_str())
        }
        _ => format!("canonical AAD rule {}", rule.as_str()),
    };
    crate::error(ErrorKind::InvalidArgument, Operation::Construct, message)
        .with_detail(anykms_core::BackendDetail::new().with_code(rule.code()))
}

/// `[a-z][a-z0-9<extra>]*`, usable in constants.
const fn is_name(s: &[u8], extra: u8) -> bool {
    if s.is_empty() || !s[0].is_ascii_lowercase() {
        return false;
    }
    let mut i = 1;
    while i < s.len() {
        let b = s[i];
        if !(b.is_ascii_lowercase() || b.is_ascii_digit() || b == extra) {
            return false;
        }
        i += 1;
    }
    true
}

const fn eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

fn check_purpose(purpose: &str) -> Result<()> {
    let rule = if purpose.is_empty() {
        Rule::EmptyPurpose
    } else if purpose.len() > MAX_PURPOSE_LEN {
        Rule::PurposeTooLong
    } else if !is_name(purpose.as_bytes(), b'-') {
        Rule::PurposeCharset
    } else {
        return Ok(());
    };
    Err(violation(rule, None))
}

fn check_key(key: &str) -> Result<()> {
    let rule = if key.is_empty() {
        Rule::EmptyKey
    } else if key.len() > MAX_KEY_LEN {
        Rule::KeyTooLong
    } else if !is_name(key.as_bytes(), b'_') {
        Rule::KeyCharset
    } else if key == PURPOSE_KEY {
        Rule::ReservedKey
    } else {
        return Ok(());
    };
    Err(violation(rule, Some(key)))
}

fn check_value(key: &str, value: &str) -> Result<()> {
    let rule = if value.is_empty() {
        Rule::EmptyValue
    } else if value.len() > MAX_VALUE_LEN {
        Rule::ValueTooLong
    } else if value.chars().any(char::is_control) {
        Rule::ValueControlChar
    } else {
        return Ok(());
    };
    Err(violation(rule, Some(key)))
}

/// What a purpose declaration must satisfy; evaluated at compile time by [`Aad::builder`].
const fn valid_declaration(name: &str, fields: &[&str]) -> bool {
    let name = name.as_bytes();
    if name.len() > MAX_PURPOSE_LEN || !is_name(name, b'-') || fields.len() > MAX_FIELDS {
        return false;
    }
    let mut i = 0;
    while i < fields.len() {
        let key = fields[i].as_bytes();
        if key.len() > MAX_KEY_LEN || !is_name(key, b'_') || eq(key, PURPOSE_KEY.as_bytes()) {
            return false;
        }
        let mut j = 0;
        while j < i {
            if eq(key, fields[j].as_bytes()) {
                return false;
            }
            j += 1;
        }
        i += 1;
    }
    true
}

/// A kind of record: its canonical AAD purpose name, the fields that bind a ciphertext to its row,
/// and the padding its envelopes use. Implemented by hand next to the record type, together with
/// `impl TryFrom<&Record> for Aad` built with [`Aad::builder`].
///
/// An invalid declaration (a name or key outside the v1 charsets or caps, the reserved key
/// `purpose`, a duplicate key, more than 15 fields) does not compile once a builder uses it:
///
/// ```compile_fail,E0080
/// struct Bad;
/// impl kekring::Purpose for Bad {
///     const NAME: &'static str = "Bad_Name";
///     const FIELDS: &'static [&'static str] = &["id"];
/// }
/// let _ = kekring::Aad::builder::<Bad>();
/// ```
///
/// ```compile_fail,E0080
/// struct Reserved;
/// impl kekring::Purpose for Reserved {
///     const NAME: &'static str = "reserved";
///     const FIELDS: &'static [&'static str] = &["id", "purpose"];
/// }
/// let _ = kekring::Aad::builder::<Reserved>();
/// ```
///
/// A valid one does:
///
/// ```
/// struct Note<'a> { id: &'a str }
/// impl kekring::Purpose for Note<'_> {
///     const NAME: &'static str = "note";
///     const FIELDS: &'static [&'static str] = &["id"];
/// }
/// impl TryFrom<&Note<'_>> for kekring::Aad {
///     type Error = anykms_core::Error;
///     fn try_from(n: &Note<'_>) -> anykms_core::Result<Self> {
///         kekring::Aad::builder::<Note>().str("id", n.id).build()
///     }
/// }
/// let aad = kekring::Aad::try_from(&Note { id: "42" }).unwrap();
/// assert_eq!(aad.purpose(), "note");
/// ```
pub trait Purpose {
    /// The purpose name: 1..=48 bytes of `[a-z][a-z0-9-]*`.
    const NAME: &'static str;
    /// The application field keys, each 1..=32 bytes of `[a-z][a-z0-9_]*`, at most 15. Every one
    /// is required.
    const FIELDS: &'static [&'static str];
    /// The padding new envelopes of this purpose use.
    const PADDING: Padding = Padding::NONE;
}

/// Canonical AAD v1 bytes, with the purpose and padding they were built for.
#[derive(Clone, PartialEq, Eq)]
pub struct Aad {
    bytes: Vec<u8>,
    purpose: &'static str,
    padding: Padding,
}

impl Aad {
    /// The specification's encoder: the canonical bytes of `purpose` over `fields` (any order),
    /// or `InvalidArgument` naming the first rule the input breaks (`aad:<rule>` as the detail
    /// code). Prefer the typed [`builder`](Self::builder).
    ///
    /// # Errors
    /// `InvalidArgument` for any rule in the specification's table.
    pub fn encode(purpose: &str, fields: &[(&str, &str)]) -> Result<Vec<u8>> {
        check_purpose(purpose)?;
        if fields.len() > MAX_FIELDS {
            return Err(violation(Rule::TooManyFields, None));
        }
        let mut all = Vec::with_capacity(fields.len() + 1);
        for &(key, value) in fields {
            check_key(key)?;
            check_value(key, value)?;
            all.push((key, value));
        }
        all.push((PURPOSE_KEY, purpose));
        all.sort_unstable_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        if let Some(w) = all.windows(2).find(|w| w[0].0 == w[1].0) {
            return Err(violation(Rule::DuplicateKey, Some(w[0].0)));
        }
        // Every length below is capped well under u16::MAX by the rules above.
        let len16 = |n: usize| {
            u16::try_from(n)
                .expect("bounded by the rules")
                .to_be_bytes()
        };
        let mut out = LABEL.to_vec();
        out.extend_from_slice(&len16(all.len()));
        for (key, value) in &all {
            out.extend_from_slice(&len16(key.len()));
            out.extend_from_slice(key.as_bytes());
            out.extend_from_slice(&len16(value.len()));
            out.extend_from_slice(value.as_bytes());
        }
        if out.len() > MAX_TOTAL_LEN {
            return Err(violation(Rule::TotalTooLong, None));
        }
        Ok(out)
    }

    /// A builder for `P`, whose declaration is checked at compile time (see [`Purpose`]).
    #[must_use]
    pub fn builder<P: Purpose + ?Sized>() -> AadBuilder {
        const {
            assert!(
                valid_declaration(P::NAME, P::FIELDS),
                "invalid kekring::Purpose declaration"
            )
        };
        AadBuilder {
            purpose: P::NAME,
            declared: P::FIELDS,
            padding: P::PADDING,
            fields: Vec::with_capacity(P::FIELDS.len()),
        }
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn purpose(&self) -> &'static str {
        self.purpose
    }

    #[must_use]
    pub fn padding(&self) -> Padding {
        self.padding
    }
}

/// Values are bound, not shown.
impl fmt::Debug for Aad {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Aad")
            .field("purpose", &self.purpose)
            .field("padding", &self.padding)
            .finish_non_exhaustive()
    }
}

/// Collects the declared fields of one purpose; [`build`](Self::build) encodes them.
pub struct AadBuilder {
    purpose: &'static str,
    declared: &'static [&'static str],
    padding: Padding,
    fields: Vec<(&'static str, String)>,
}

impl AadBuilder {
    /// A string field, byte-exact.
    #[must_use]
    pub fn str(mut self, key: &'static str, value: impl AsRef<str>) -> Self {
        self.fields.push((key, value.as_ref().to_owned()));
        self
    }

    /// An integer field, as canonical decimal text.
    #[must_use]
    pub fn u64(mut self, key: &'static str, value: u64) -> Self {
        self.fields.push((key, value.to_string()));
        self
    }

    /// # Errors
    /// `InvalidArgument` for an undeclared, duplicate or missing field and for any rule of the
    /// encoding; the error names the rule and the key, never a value.
    pub fn build(self) -> Result<Aad> {
        for (key, _) in &self.fields {
            if !self.declared.contains(key) {
                return Err(violation(Rule::UndeclaredField, Some(key)));
            }
        }
        for key in self.declared {
            if !self.fields.iter().any(|(k, _)| k == key) {
                return Err(violation(Rule::MissingField, Some(key)));
            }
        }
        let fields: Vec<(&str, &str)> = self.fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
        Ok(Aad {
            bytes: Aad::encode(self.purpose, &fields)?,
            purpose: self.purpose,
            padding: self.padding,
        })
    }
}

impl fmt::Debug for AadBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AadBuilder")
            .field("purpose", &self.purpose)
            .field(
                "keys",
                &self.fields.iter().map(|(k, _)| k).collect::<Vec<_>>(),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations() {
        assert!(valid_declaration("session", &["session_id"]));
        assert!(valid_declaration("a", &[]));
        assert!(!valid_declaration("", &[]));
        assert!(!valid_declaration("Session", &[]));
        assert!(!valid_declaration("a_b", &[]));
        assert!(!valid_declaration(&"a".repeat(49), &[]));
        assert!(!valid_declaration("a", &["purpose"]));
        assert!(!valid_declaration("a", &["x", "x"]));
        assert!(!valid_declaration("a", &["x-y"]));
        assert!(!valid_declaration("a", &["1x"]));
        assert!(!valid_declaration("a", &[""]));
        let sixteen: Vec<String> = (0..16).map(|i| format!("k{i}")).collect();
        let keys: Vec<&str> = sixteen.iter().map(String::as_str).collect();
        assert!(valid_declaration("a", &keys[..15]));
        assert!(!valid_declaration("a", &keys));
    }
}
