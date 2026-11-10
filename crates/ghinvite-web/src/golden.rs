//! Golden fixtures for persisted serde shapes (plan GM-00, PD-9). Fixtures live
//! in `golden/` and are only compared; `GOLDEN_BLESS=1` rewrites them.
use serde::{Serialize, de::DeserializeOwned};

fn path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("golden")
        .join(name)
}

/// Compare `actual` with the fixture text, returning the fixture.
pub(crate) fn text(name: &str, actual: &str) -> String {
    let path = path(name);
    if std::env::var_os("GOLDEN_BLESS").is_some_and(|value| value == "1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("{actual}\n")).unwrap();
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "missing golden {} ({error}); bless with GOLDEN_BLESS=1",
            path.display()
        )
    });
    let expected = expected.strip_suffix('\n').unwrap_or(&expected).to_owned();
    assert_eq!(
        actual,
        expected,
        "persisted shape changed (PD-9): {}",
        path.display()
    );
    expected
}

/// Production writes these values with `serde_json`. The fixture is the same
/// serializer's pretty form, so field order and encodings are compared
/// exactly; the fixture must also decode and re-encode to itself.
pub(crate) fn json<T: Serialize + DeserializeOwned>(name: &str, value: &T) -> T {
    let compact: serde_json::Value =
        serde_json::from_slice(&serde_json::to_vec(value).unwrap()).unwrap();
    let fixture = text(name, &serde_json::to_string_pretty(value).unwrap());
    assert_eq!(
        compact,
        serde_json::from_str::<serde_json::Value>(&fixture).unwrap()
    );
    let decoded: T = serde_json::from_str(&fixture).unwrap();
    assert_eq!(
        serde_json::to_string_pretty(&decoded).unwrap(),
        fixture,
        "fixture does not round-trip: {name}"
    );
    decoded
}
