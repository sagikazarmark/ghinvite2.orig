//! anykms 0.4 conformance for `KekringProvider` (registry path) and `ProviderKey` (typed path).
//!
//! A keyring is immutable for the life of a process (rotation is a new descriptor and a
//! redeploy), so the fixture provisions every keyring before the run, as an operator would, and
//! hands them out one per `create_key`. Rotating a key under an open handle, which the
//! `RotateKeyWrap` cases need, is not something this backend can do by design; kekring's own
//! tests cover rotation across descriptors.
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anykms_conformance::{
    Feature, Fixture, FixtureCaps, KeyUse, KeyWrapKeyHandles, TestKey, TestKeyKind, TypedHandles,
};
use anykms_core::{Error, KeyUri, Provider, Result, SignatureAlgorithm};
use kekring::{KekringProvider, Keyring};

/// More than one run asks for.
const PROVISIONED: usize = 256;

struct KekringFixture {
    provider: KekringProvider,
    next: AtomicUsize,
}

impl KekringFixture {
    fn new() -> Self {
        let rings = (0..PROVISIONED).map(|i| {
            let mut root = [0u8; 32];
            getrandom::fill(&mut root).expect("operating system RNG");
            Keyring::builder(&format!("conformance-{i}"))
                .generation(1, root)
                .current(1)
                .build()
                .expect("valid keyring")
        });
        Self {
            provider: KekringProvider::new(rings).expect("unique names"),
            next: AtomicUsize::new(0),
        }
    }
}

struct Typed(KekringProvider);

impl TypedHandles for Typed {
    fn key_wrap(&self, uri: &KeyUri) -> Result<KeyWrapKeyHandles> {
        let k = Arc::new(self.0.key_wrap_key(uri)?);
        let semantics = anykms_core::Semantics::semantics(&*k).cloned();
        let h = KeyWrapKeyHandles::bridge(k);
        Ok(match semantics {
            Some(s) => h.with_semantics(s),
            None => h,
        })
    }
}

impl Fixture for KekringFixture {
    fn name(&self) -> &'static str {
        "kekring"
    }

    async fn probe(&self) -> FixtureCaps {
        use Feature::{
            Aead, BackendInfo, DataKeys, Describe, DescribeVersionState, Disable, DisableVersion,
            EmptyMessage, EmptyPlaintext, Extension, KeyWrap, KeyWrapPublicKeys, KeyWrapRewrap,
            Mac, MacInputLimit, MessageInputLimit, MinDecryptionVersion, NativeCiphertext,
            OversizePlaintext, PreflightState, RequestCount, Rewrap, RewrapWithAad, Rotate,
            RotateKeyWrap, RotateMac, RotateSigning, ServerErrorMessages, Signing, Throttle,
            UnauthorizedProvider, UsedPrimary, WrappedDataKeys,
        };
        let mut caps = FixtureCaps::new()
            .supports_all(&[KeyWrap, Describe])
            .not_applicable_all(
                &[Aead, Rotate, Rewrap, RewrapWithAad, EmptyPlaintext, OversizePlaintext, NativeCiphertext, UsedPrimary],
                "a key-wrapping key only: no AEAD keys",
            )
            .not_applicable_all(
                &[DataKeys, WrappedDataKeys],
                "no native data keys (the recipes wrap their own)",
            )
            .not_applicable(
                KeyWrapRewrap,
                "no server-side rewrap by design: EnvelopeAead::rewrap skips the key-commitment check",
            )
            .not_applicable(
                RotateKeyWrap,
                "a keyring is immutable for the process: rotation is a new descriptor and a redeploy",
            )
            .not_applicable(KeyWrapPublicKeys, "a symmetric key-wrapping key")
            .not_applicable_all(&[Mac, RotateMac, MacInputLimit, EmptyMessage], "no MAC keys")
            .not_applicable_all(&[RotateSigning, MessageInputLimit], "no signing keys")
            .not_applicable_all(
                &[Disable, DisableVersion, PreflightState, DescribeVersionState, MinDecryptionVersion],
                "no key or version state: a generation is listed or removed by the descriptor",
            )
            .not_applicable_all(
                &[Throttle, ServerErrorMessages, UnauthorizedProvider, RequestCount],
                "in process: no server, quotas, server messages, credentials or requests",
            )
            .not_applicable_all(&[Extension, BackendInfo], "not implemented");
        for &alg in SignatureAlgorithm::ALL {
            caps = caps.not_applicable(Signing(alg), "no signing keys");
        }
        caps
    }

    async fn create_key(&self, kind: TestKeyKind, _: KeyUse) -> Result<TestKey> {
        if kind != TestKeyKind::KeyWrap {
            return Err(Error::unsupported(format!(
                "{kind:?} keys are declared n/a"
            )));
        }
        let i = self.next.fetch_add(1, Ordering::Relaxed);
        if i >= PROVISIONED {
            return Err(Error::unsupported("every provisioned keyring is in use"));
        }
        let name = format!("conformance-{i}");
        Ok(TestKey::new(
            KeyUri::parse(&format!("kekring:{name}"))?,
            name,
            kind,
        ))
    }

    async fn rotate(&self, _: &TestKey) -> Result<TestKey> {
        Err(Error::unsupported("RotateKeyWrap is declared n/a"))
    }

    fn missing_key_uri(&self, _: TestKeyKind) -> KeyUri {
        KeyUri::parse("kekring:missing").expect("valid URI")
    }

    fn provider(&self) -> Arc<dyn Provider> {
        Arc::new(self.provider.clone())
    }

    fn typed(&self) -> Arc<dyn TypedHandles> {
        Arc::new(Typed(self.provider.clone()))
    }
}

fn main() -> ExitCode {
    anykms_conformance::run(async || KekringFixture::new())
}
