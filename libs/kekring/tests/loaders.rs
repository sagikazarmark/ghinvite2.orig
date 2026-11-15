#![cfg(feature = "std-loaders")]
use kekring::Keyring;

fn descriptor() -> String {
    Keyring::builder("loaders")
        .generation(1, [1; 32])
        .current(1)
        .build()
        .unwrap()
        .to_descriptor()
        .to_string()
}

#[test]
fn file_permissions() {
    let path = std::env::temp_dir().join(format!("keyring-loader-{}", std::process::id()));
    std::fs::write(&path, descriptor() + "\n").unwrap();
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
    if std::env::var_os("KEKRING_LOADER_TEST_CHILD").is_some() {
        assert_eq!(
            Keyring::from_env("KEKRING_LOADER_TEST_VALUE")
                .unwrap()
                .name(),
            "loaders"
        );
        assert!(Keyring::from_env("KEKRING_LOADER_TEST_MISSING").is_err());
        return;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "environment"])
        .env("KEKRING_LOADER_TEST_CHILD", "1")
        .env("KEKRING_LOADER_TEST_VALUE", descriptor())
        .env_remove("KEKRING_LOADER_TEST_MISSING")
        .status()
        .unwrap();
    assert!(status.success());
}
