#![cfg(feature = "std-loaders")]
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use kekring::Keyring;
use std::fmt::Write;
fn descriptor() -> String {
    let k = Keyring::builder("loaders")
        .generation(1, [1; 32])
        .unwrap()
        .current(1)
        .unwrap()
        .build()
        .unwrap();
    let mut kcv = String::new();
    for byte in k.key_check_value(1).unwrap() {
        write!(kcv, "{byte:02x}").unwrap();
    }
    format!(
        "akr1;name=loaders;current=1;k1={}.{kcv}",
        URL_SAFE_NO_PAD.encode([1; 32])
    )
}
#[test]
fn file_permissions() {
    let path = std::env::temp_dir().join(format!("keyring-loader-{}", std::process::id()));
    std::fs::write(&path, descriptor()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Keyring::from_file(&path).is_err());
        assert!(Keyring::from_file_with_permissions(&path, true).is_ok());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert_eq!(Keyring::from_file(&path).unwrap().name(), "loaders");
    std::fs::remove_file(&path).unwrap();
    assert!(Keyring::from_file(&path).is_err());
}
#[test]
fn environment() {
    if std::env::var_os("ANYKMS_LOADER_TEST_CHILD").is_some() {
        assert_eq!(
            Keyring::from_env("ANYKMS_LOADER_TEST_VALUE")
                .unwrap()
                .name(),
            "loaders"
        );
        assert!(Keyring::from_env("ANYKMS_LOADER_TEST_MISSING").is_err());
        return;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "environment"])
        .env("ANYKMS_LOADER_TEST_CHILD", "1")
        .env("ANYKMS_LOADER_TEST_VALUE", descriptor())
        .env_remove("ANYKMS_LOADER_TEST_MISSING")
        .status()
        .unwrap();
    assert!(status.success());
}
