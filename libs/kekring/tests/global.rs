#![cfg(feature = "global")]
use anykms_core::ErrorKind;
use kekring::{Keyring, global};
use std::sync::Arc;
#[test]
fn global_install_is_idempotent_and_rejects_conflict() {
    assert_eq!(global::get().unwrap_err().kind(), ErrorKind::Unavailable);
    let ring = Arc::new(
        Keyring::builder("global")
            .generation(1, [1; 32])
            .unwrap()
            .current(1)
            .unwrap()
            .build()
            .unwrap(),
    );
    global::install(ring.clone()).unwrap();
    global::install(ring).unwrap();
    let other = Arc::new(
        Keyring::builder("global")
            .generation(1, [2; 32])
            .unwrap()
            .current(1)
            .unwrap()
            .build()
            .unwrap(),
    );
    assert_eq!(
        global::install(other).unwrap_err().kind(),
        ErrorKind::Configuration
    );
    assert_eq!(global::get().unwrap().current_version(), 1);
}
