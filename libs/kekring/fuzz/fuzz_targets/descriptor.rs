#![no_main]
//! The descriptor parser: never panics, and the grammar is canonical, so any accepted input is
//! exactly the keyring's own descriptor (less one optional trailing newline).
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: &[u8]| {
    let Ok(text) = std::str::from_utf8(input) else {
        return;
    };
    match kekring::Keyring::parse(text) {
        Ok(ring) => {
            assert_eq!(
                &*ring.to_descriptor(),
                text.strip_suffix('\n').unwrap_or(text)
            );
            assert!(ring.versions().len() <= kekring::MAX_GENERATIONS);
            assert!(ring.high_water_mark() >= ring.current_version());
        }
        Err(e) => {
            assert_eq!(e.kind(), anykms_core::ErrorKind::Configuration);
            let code = e.detail().and_then(|d| d.code()).unwrap_or_default();
            assert!(code.starts_with("descriptor:"), "{code}");
        }
    }
});
