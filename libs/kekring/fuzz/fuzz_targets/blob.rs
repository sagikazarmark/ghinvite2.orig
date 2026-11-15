#![no_main]
//! The wrapped-key reader over a keyring with a removed generation: never panics, fails only with
//! the documented kinds and codes, and accepts only the blob it would write itself (AES-KW is
//! deterministic and its integrity check binds the body to one KEK).
use anykms_core::{Ciphertext, ErrorKind, KeyWrap};
use kekring::{Code, Keyring};
use libfuzzer_sys::fuzz_target;
use std::sync::LazyLock;
use std::task::{Context, Poll, Waker};

/// Generations 1 and 3, 3 current, high-water mark 4: 2 removed, 4 retired, 5+ unknown.
static RING: LazyLock<Keyring> = LazyLock::new(|| {
    Keyring::builder("fuzz")
        .generation(1, [1; 32])
        .generation(3, [3; 32])
        .current(3)
        .high_water_mark(4)
        .build()
        .unwrap()
});

fn ready<T>(f: impl Future<Output = T>) -> T {
    let mut f = std::pin::pin!(f);
    match f.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("kekring futures complete on their first poll"),
    }
}

fuzz_target!(|input: &[u8]| {
    let kek = RING.kek();
    match ready(kek.unwrap_key(&Ciphertext::from(input.to_vec()))) {
        Ok(dek) => {
            assert_eq!(dek.len(), kekring::DEK_LEN);
            let version = u32::from_be_bytes(input[4..8].try_into().unwrap());
            if version == RING.current_version() {
                assert_eq!(ready(kek.wrap_key(&dek)).unwrap().as_bytes(), input);
            }
        }
        Err(e) => {
            assert!(e.retry_hint().is_none());
            match (e.kind(), Code::of(&e)) {
                (
                    ErrorKind::DecryptFailed,
                    None | Some(Code::VersionAboveHighWaterMark | Code::KeyIdMismatch),
                ) => {}
                (ErrorKind::KeyVersionUnusable, Some(Code::GenerationRemoved)) => {}
                other => panic!("undocumented failure {other:?}"),
            }
        }
    }
});
