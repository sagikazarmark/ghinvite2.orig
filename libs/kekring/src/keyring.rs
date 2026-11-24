//! The keyring: generations derived from provisioned roots, the descriptor that carries them, and
//! the builder both paths go through. Format: `docs/kekring-format.md`.
use crate::{Error, ErrorKind, Operation, Result, labels};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fmt, sync::Arc};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

/// The most generations a keyring holds (policy; the worst-case descriptor stays under 5 KiB).
pub const MAX_GENERATIONS: usize = 64;
/// The longest keyring name, in bytes (policy; well under the `u8` length prefix and the
/// envelope's 255-byte `kek_id`).
pub const MAX_NAME_LEN: usize = 64;
/// A descriptor longer than this is refused before it is split (policy; the longest valid one is
/// 4785 bytes, see the format document).
const MAX_DESCRIPTOR_LEN: usize = 8192;
const ROOT_B64_LEN: usize = 43;
const KID_HEX_LEN: usize = 16;

fn config(rule: &'static str, message: impl Into<String>) -> Error {
    crate::error(ErrorKind::Configuration, Operation::Configure, message)
        .with_detail(anykms_core::BackendDetail::new().with_code(rule))
}

/// `[a-z0-9][a-z0-9_-]*`, 1..=64 bytes: no `;`, `=`, `.`, whitespace or control characters.
pub(crate) fn valid_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= MAX_NAME_LEN
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

/// `[1-9][0-9]*` within `u32`.
fn version(s: &str) -> Option<u32> {
    (!s.starts_with('0') && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse::<u32>().ok())
        .flatten()
}

pub(crate) fn hex(b: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    b.iter()
        .flat_map(|b| {
            [
                char::from(DIGITS[usize::from(b >> 4)]),
                char::from(DIGITS[usize::from(b & 15)]),
            ]
        })
        .collect()
}

fn name_len(name: &str) -> u8 {
    u8::try_from(name.len()).expect("validated keyring name is at most 64 bytes")
}

/// `label ‖ u8 len ‖ name ‖ u32be version`.
fn info(label: &[u8], name: &str, v: u32) -> Vec<u8> {
    let mut b = Vec::with_capacity(label.len() + 1 + name.len() + 4);
    b.extend_from_slice(label);
    b.push(name_len(name));
    b.extend_from_slice(name.as_bytes());
    b.extend_from_slice(&v.to_be_bytes());
    b
}

pub(crate) struct Generation {
    root: Zeroizing<[u8; 32]>,
    pub(crate) kek: Zeroizing<[u8; 32]>,
    pub(crate) kid: [u8; 8],
}

fn derive(name: &str, v: u32, root: Zeroizing<[u8; 32]>) -> Generation {
    let hk = Hkdf::<Sha256>::new(Some(labels::SALT), &root[..]);
    let mut kek = Zeroizing::new([0; 32]);
    let mut kid = [0; 8];
    hk.expand(&info(labels::KEK, name, v), &mut kek[..])
        .expect("valid HKDF length");
    hk.expand(&info(labels::KID, name, v), &mut kid)
        .expect("valid HKDF length");
    Generation { root, kek, kid }
}

pub(crate) struct Material {
    pub(crate) name: String,
    pub(crate) current: u32,
    pub(crate) hwm: u32,
    pub(crate) gens: BTreeMap<u32, Generation>,
}

impl Material {
    pub(crate) fn current(&self) -> (u32, &Generation) {
        (self.current, &self.gens[&self.current])
    }
}

/// A versioned key-encryption key provisioned from one secret descriptor: generations, the
/// current one that wraps, and the high-water mark (the highest version ever issued). Immutable
/// for its lifetime; a change is a new descriptor and a redeploy.
///
/// `Debug` and [`fingerprint`](Self::fingerprint) show identifiers only, never roots or KEKs.
#[derive(Clone)]
pub struct Keyring(pub(crate) Arc<Material>);

/// The public parts of a keyring and an 8-byte digest over them, for logs and rollout checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    pub name: String,
    pub current: u32,
    pub high_water_mark: u32,
    pub versions: Vec<u32>,
    pub digest: String,
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} current={} hwm={} versions={} fp={}",
            self.name,
            self.current,
            self.high_water_mark,
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

/// Collects generations; [`build`](Self::build) checks every rule the descriptor parser checks
/// after syntax, so both paths reject the same keyrings.
pub struct KeyringBuilder {
    name: String,
    current: Option<u32>,
    hwm: Option<u32>,
    roots: Vec<(u32, Zeroizing<[u8; 32]>)>,
}

impl KeyringBuilder {
    #[must_use]
    pub fn generation(mut self, v: u32, root: impl Into<Zeroizing<[u8; 32]>>) -> Self {
        self.roots.push((v, root.into()));
        self
    }

    #[must_use]
    pub fn current(mut self, v: u32) -> Self {
        self.current = Some(v);
        self
    }

    /// The highest version ever issued. Defaults to the highest listed version; set it once a
    /// removed generation was above every remaining one.
    #[must_use]
    pub fn high_water_mark(mut self, v: u32) -> Self {
        self.hwm = Some(v);
        self
    }

    /// # Errors
    /// `Configuration` (with a `descriptor:<rule>` detail code) for an invalid name, an empty
    /// ring or more than 64 generations, a zero or repeated version, an all-zero or repeated
    /// root, a missing or undeclared current, or a high-water mark below a listed version.
    pub fn build(self) -> Result<Keyring> {
        if !valid_name(&self.name) {
            return Err(config("descriptor:name", "keyring name is invalid"));
        }
        if self.roots.is_empty() || self.roots.len() > MAX_GENERATIONS {
            return Err(config(
                "descriptor:generation_count",
                "keyring must contain 1 to 64 generations",
            ));
        }
        let mut roots = BTreeMap::new();
        for (v, root) in self.roots {
            if v == 0 {
                return Err(config(
                    "descriptor:version",
                    "generation version must be nonzero",
                ));
            }
            if roots.contains_key(&v) {
                return Err(config(
                    "descriptor:duplicate_version",
                    format!("k{v}: version listed twice"),
                ));
            }
            if root.iter().all(|b| *b == 0) {
                return Err(config(
                    "descriptor:zero_root",
                    format!("k{v}: root is all zero"),
                ));
            }
            if roots
                .values()
                .any(|r: &Zeroizing<[u8; 32]>| bool::from(r[..].ct_eq(&root[..])))
            {
                return Err(config(
                    "descriptor:duplicate_root",
                    format!("k{v}: root repeats another generation's"),
                ));
            }
            roots.insert(v, root);
        }
        let current = self
            .current
            .ok_or_else(|| config("descriptor:current", "current is missing"))?;
        if !roots.contains_key(&current) {
            return Err(config(
                "descriptor:current",
                format!("current {current} does not name a listed generation"),
            ));
        }
        let highest = *roots.keys().next_back().expect("nonempty");
        let hwm = self.hwm.unwrap_or(highest);
        if hwm < highest {
            return Err(config(
                "descriptor:high_water_mark",
                format!("high-water mark {hwm} is below listed version {highest}"),
            ));
        }
        let gens = roots
            .into_iter()
            .map(|(v, root)| (v, derive(&self.name, v, root)))
            .collect();
        Ok(Keyring(Arc::new(Material {
            name: self.name,
            current,
            hwm,
            gens,
        })))
    }
}

impl fmt::Debug for KeyringBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyringBuilder")
            .field("name", &self.name)
            .field("current", &self.current)
            .field("high_water_mark", &self.hwm)
            .field(
                "versions",
                &self.roots.iter().map(|(v, _)| v).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl Keyring {
    #[must_use]
    pub fn builder(name: &str) -> KeyringBuilder {
        KeyringBuilder {
            name: name.into(),
            current: None,
            hwm: None,
            roots: Vec::new(),
        }
    }

    /// Parses a descriptor:
    /// `kekring1;name=NAME;current=N;hwm=H;kV=ROOT.KID[;kV=ROOT.KID]...`, fields in exactly this
    /// order, generations ascending, at most one trailing `\n`. `ROOT` is 32 bytes of unpadded
    /// base64url; `KID` is the generation's 8-byte key id in lowercase hex, checked against the
    /// root.
    ///
    /// # Errors
    /// `Configuration` with a `descriptor:<rule>` detail code. Messages name fields and versions,
    /// never root material, and carry no source.
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.strip_suffix('\n').unwrap_or(input);
        if input.len() > MAX_DESCRIPTOR_LEN {
            return Err(config("descriptor:length", "descriptor is too long"));
        }
        if !input.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(config(
                "descriptor:charset",
                "descriptor contains whitespace, control or non-ASCII characters",
            ));
        }
        let mut fields = input.split(';');
        if fields.next() != Some(labels::DESCRIPTOR_PREFIX) {
            return Err(config(
                "descriptor:prefix",
                "descriptor must start with kekring1",
            ));
        }
        let mut header = |key: &'static str| {
            fields
                .next()
                .and_then(|f| f.strip_prefix(key))
                .and_then(|f| f.strip_prefix('='))
                .ok_or_else(|| {
                    config(
                        "descriptor:field_order",
                        format!("expected field {key} (order: name, current, hwm, generations)"),
                    )
                })
        };
        let name = header("name")?;
        let current = header("current")?;
        let hwm = header("hwm")?;
        if !valid_name(name) {
            return Err(config("descriptor:name", "keyring name is invalid"));
        }
        let bad_version = |field: &str| {
            config(
                "descriptor:version",
                format!("{field} must be a canonical nonzero u32"),
            )
        };
        let current = version(current).ok_or_else(|| bad_version("current"))?;
        let hwm = version(hwm).ok_or_else(|| bad_version("hwm"))?;
        let mut builder = Keyring::builder(name).current(current).high_water_mark(hwm);
        let mut kids = Vec::new();
        let mut previous = 0;
        for field in fields {
            let (key, value) = field
                .split_once('=')
                .filter(|(k, _)| k.starts_with('k'))
                .ok_or_else(|| {
                    config("descriptor:field_order", "expected a generation field kN")
                })?;
            let v = version(&key[1..]).ok_or_else(|| bad_version("generation version"))?;
            if v <= previous {
                return Err(config(
                    "descriptor:generation_order",
                    format!("k{v}: generations must be strictly ascending"),
                ));
            }
            previous = v;
            if kids.len() == MAX_GENERATIONS {
                return Err(config(
                    "descriptor:generation_count",
                    "keyring must contain 1 to 64 generations",
                ));
            }
            let (encoded, kid) = value.split_once('.').unwrap_or((value, ""));
            let mut root = Zeroizing::new([0; 32]);
            if encoded.len() != ROOT_B64_LEN
                || URL_SAFE_NO_PAD
                    .decode_slice(encoded, &mut root[..])
                    .ok()
                    .filter(|n| *n == 32)
                    .is_none()
            {
                return Err(config(
                    "descriptor:root",
                    format!("k{v}: root is not 32 bytes of canonical unpadded base64url"),
                ));
            }
            if kid.len() != KID_HEX_LEN
                || !kid
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(config(
                    "descriptor:kid",
                    format!("k{v}: key id must be 16 lowercase hex characters"),
                ));
            }
            kids.push((v, kid));
            builder = builder.generation(v, root);
        }
        if kids.is_empty() {
            return Err(config(
                "descriptor:generation_count",
                "keyring must contain 1 to 64 generations",
            ));
        }
        let ring = builder.build()?;
        for (v, kid) in kids {
            let derived = hex(&ring.0.gens[&v].kid);
            if !bool::from(derived.as_bytes().ct_eq(kid.as_bytes())) {
                return Err(config(
                    "descriptor:kid_mismatch",
                    format!("k{v}: key id does not match the root"),
                ));
            }
        }
        Ok(ring)
    }

    /// The canonical descriptor (no trailing newline). It contains every root: treat it as the
    /// secret it is.
    #[must_use]
    pub fn to_descriptor(&self) -> Zeroizing<String> {
        let m = &self.0;
        // Reserved up front so the roots are never left behind in a reallocated buffer.
        let mut out = Zeroizing::new(String::with_capacity(
            128 + m.name.len() + m.gens.len() * (13 + ROOT_B64_LEN + 1 + KID_HEX_LEN),
        ));
        out.push_str(&format!(
            "{};name={};current={};hwm={}",
            labels::DESCRIPTOR_PREFIX,
            m.name,
            m.current,
            m.hwm
        ));
        let mut root = Zeroizing::new([0u8; ROOT_B64_LEN]);
        for (v, g) in &m.gens {
            URL_SAFE_NO_PAD
                .encode_slice(&g.root[..], &mut root[..])
                .expect("32 bytes encode to 43 characters");
            out.push_str(&format!(";k{v}="));
            out.push_str(std::str::from_utf8(&root[..]).expect("base64url is ASCII"));
            out.push('.');
            out.push_str(&hex(&g.kid));
        }
        out
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
    pub fn high_water_mark(&self) -> u32 {
        self.0.hwm
    }

    /// The retained generations, ascending.
    pub fn versions(&self) -> impl ExactSizeIterator<Item = u32> + '_ {
        self.0.gens.keys().copied()
    }

    /// A generation's key id: the selector its wrapped keys carry. Public, not secret.
    #[must_use]
    pub fn kid(&self, v: u32) -> Option<[u8; 8]> {
        self.0.gens.get(&v).map(|g| g.kid)
    }

    pub(crate) fn root(&self, v: u32) -> Option<&[u8; 32]> {
        self.0.gens.get(&v).map(|g| &*g.root)
    }

    /// SHA-256 over `kekring/v1/fp ‖ u8 len ‖ name ‖ u32be current ‖ u32be hwm ‖
    /// (u32be version ‖ kid)*`, first 8 bytes in hex. Never covers roots.
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        let m = &self.0;
        let mut h = Sha256::new();
        h.update(labels::FP);
        h.update([name_len(&m.name)]);
        h.update(m.name.as_bytes());
        h.update(m.current.to_be_bytes());
        h.update(m.hwm.to_be_bytes());
        for (v, g) in &m.gens {
            h.update(v.to_be_bytes());
            h.update(g.kid);
        }
        Fingerprint {
            name: m.name.clone(),
            current: m.current,
            high_water_mark: m.hwm,
            versions: self.versions().collect(),
            digest: hex(&h.finalize()[..8]),
        }
    }

    #[cfg(feature = "std-loaders")]
    /// # Errors
    /// `Configuration` if the variable is absent, not UTF-8, or not a valid descriptor.
    pub fn from_env(var: &str) -> Result<Self> {
        let value = Zeroizing::new(std::env::var(var).map_err(|_| {
            config(
                "loader:environment",
                "cannot read keyring environment variable",
            )
        })?);
        Self::parse(&value)
    }

    #[cfg(feature = "std-loaders")]
    /// # Errors
    /// `Configuration` if the file cannot be read, is readable by group or others (Unix), or is
    /// not a valid descriptor.
    pub fn from_file(path: &std::path::Path) -> Result<Self> {
        Self::from_file_with_permissions(path, false)
    }

    #[cfg(feature = "std-loaders")]
    /// [`from_file`](Self::from_file), with `allow_insecure` skipping the Unix mode check.
    ///
    /// # Errors
    /// As [`from_file`](Self::from_file).
    pub fn from_file_with_permissions(
        path: &std::path::Path,
        allow_insecure: bool,
    ) -> Result<Self> {
        use std::io::Read;
        let file_error = |m: &'static str| config("loader:file", m);
        let mut file =
            std::fs::File::open(path).map_err(|_| file_error("cannot open keyring file"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = file
                .metadata()
                .map_err(|_| file_error("cannot inspect keyring file"))?
                .permissions()
                .mode();
            if !allow_insecure && mode & 0o077 != 0 {
                return Err(file_error("keyring file is readable by group or others"));
            }
        }
        #[cfg(not(unix))]
        let _ = allow_insecure;
        let mut value = Zeroizing::new(String::new());
        file.read_to_string(&mut value)
            .map_err(|_| file_error("cannot read keyring file"))?;
        Self::parse(&value)
    }
}
