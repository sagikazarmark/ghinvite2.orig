//! Production configuration fails before SQL or network initialization.
use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn native_web_requires_canonical_configuration_and_explicit_local_mode() {
    for (index, arguments, values, field) in [
        (
            0,
            vec![],
            vec![
                ("GHINVITE_BASE_URL", "https://legacy.example"),
                ("GHINVITE_RESTATE_AUTH", "local-unauthenticated"),
            ],
            "P1_HOST",
        ),
        (
            1,
            vec![],
            vec![("P1_HOST", "SECRET_MARKER@host")],
            "P1_HOST",
        ),
        (
            2,
            vec![],
            vec![
                ("P1_HOST", "p1.example.com"),
                ("RESTATE_INGRESS_URL", "http://127.0.0.1:8080"),
            ],
            "RESTATE_INGRESS_URL",
        ),
        (
            3,
            vec![],
            vec![
                ("P1_HOST", "p1.example.com"),
                ("RESTATE_INGRESS_URL", "https://ingress.example.com"),
                ("RESTATE_INGRESS_TOKEN", "SECRET_MARKER\n"),
            ],
            "RESTATE_INGRESS_TOKEN",
        ),
        (4, vec!["--local-dev"], vec![], "SessionKey"),
        (5, vec!["--unknown"], vec![], "usage:"),
        (
            6,
            vec![],
            vec![("P1_APP_STAGE", "SECRET_MARKER")],
            "P1_APP_STAGE",
        ),
        (7, vec![], vec![("P1_APP_STAGE", "")], "P1_APP_STAGE"),
    ] {
        let path = std::env::temp_dir().join(format!(
            "p1-web-startup-{}-{index}.sqlite",
            std::process::id()
        ));
        assert!(!path.exists());
        let mut child = Command::new(env!("CARGO_BIN_EXE_ghinvite-web"))
            .args(arguments)
            .env_clear()
            .envs(values)
            .env("GHINVITE_DATABASE_PATH", &path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                let _ = child.wait();
                panic!("startup did not fail closed");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success());
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(text.contains(field), "expected {field}: {text}");
        assert!(!text.contains("SECRET_MARKER"));
        assert!(!path.exists(), "invalid config must not initialize SQL");
    }
}
