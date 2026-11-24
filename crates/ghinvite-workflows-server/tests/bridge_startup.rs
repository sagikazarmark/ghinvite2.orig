//! Exercise the real production root, including readiness before listening.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::{
    net::{TcpListener, TcpStream},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// Whether process `pid` holds the listening socket on `address`. On Linux
/// the socket's inode is matched against the process's descriptors, so
/// another process that took the released port is not mistaken for the
/// child. A root that listened would keep serving rather than exit, so its
/// descriptors are still there to inspect. Elsewhere any listener counts.
fn listens(pid: u32, address: std::net::SocketAddr) -> bool {
    #[cfg(target_os = "linux")]
    {
        let std::net::SocketAddr::V4(v4) = address else {
            return true;
        };
        let local = format!(
            "{:08X}:{:04X}",
            u32::from_le_bytes(v4.ip().octets()),
            v4.port()
        );
        let Ok(table) = std::fs::read_to_string("/proc/net/tcp") else {
            return true;
        };
        // Columns: sl, local_address, rem_address, st (0A = LISTEN), …, inode.
        let inodes: Vec<String> = table
            .lines()
            .skip(1)
            .filter_map(|line| {
                let fields: Vec<&str> = line.split_whitespace().collect();
                (fields.get(1) == Some(&local.as_str()) && fields.get(3) == Some(&"0A"))
                    .then(|| fields.get(9).map(|inode| format!("socket:[{inode}]")))
                    .flatten()
            })
            .collect();
        let Ok(descriptors) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            return false;
        };
        descriptors.flatten().any(|fd| {
            std::fs::read_link(fd.path()).is_ok_and(|target| {
                inodes
                    .iter()
                    .any(|inode| target.as_os_str() == inode.as_str())
            })
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (pid, address);
        true
    }
}

fn rejected<V: AsRef<std::ffi::OsStr> + Copy>(
    case: &str,
    values: &[(&str, V)],
    unsigned: bool,
    expected: &str,
) {
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let key = format!(
        "fixture:{}",
        STANDARD.encode(include_bytes!("../../../test-fixtures/github-app-rsa.pem"))
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_ghinvite-workflows"));
    command
        .env_clear()
        .env("P1_HOST", "p1.example.com")
        .env("GH_REPO_APP_ID", "123")
        .env("GH_REPO_CLIENT_ID", "Iv.fixture")
        .env("GH_REPO_PRIVATE_KEYS", key)
        .env("GHINVITE_LISTEN_ADDR", address.to_string())
        .envs(values.iter().copied())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if unsigned {
        command.arg("--local-unsigned");
    } else {
        command.env(
            "RESTATE_IDENTITY_KEYS",
            "publickeyv1_ChjENKeMvCtRnqG2mrBK1HmPKufgFUc98K8B3ononQvp",
        );
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(stream) = TcpStream::connect(address) {
            // A free ephemeral port can self-connect (source port equals the
            // destination); only a connection to another socket is a listener.
            // The port is released before the child starts, so on a busy host
            // another process may take it: only the child's listener counts.
            if stream.local_addr().unwrap() != address {
                assert!(
                    !listens(child.id(), address),
                    "{case}: refused root must never listen"
                );
            }
        }
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("{case}: refusal deadline");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success(), "{case}");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text.contains(expected),
        "{case}: expected fixed refusal {expected}"
    );
    assert!(
        !text.contains("SECRET_MARKER"),
        "{case}: credential disclosure"
    );
}

#[test]
fn bridge_mode_refuses_unsigned_sqlite_implicit_or_unknown_selection() {
    rejected(
        "unsigned",
        &[("GHINVITE_STORAGE", "d1-bridge")],
        true,
        "D1 bridge forbids",
    );
    let path = std::env::temp_dir().join(format!("bridge-refused-{}.sqlite", std::process::id()));
    assert!(!path.exists());
    rejected(
        "sqlite",
        &[
            ("GHINVITE_STORAGE", "d1-bridge"),
            ("GHINVITE_DATABASE_PATH", path.to_str().unwrap()),
        ],
        false,
        "D1 bridge forbids",
    );
    assert!(!path.exists());
    rejected(
        "implicit",
        &[("GHINVITE_D1_BRIDGE_TOKEN", "SECRET_MARKER")],
        false,
        "explicit d1-bridge",
    );
    rejected(
        "unknown",
        &[("GHINVITE_STORAGE", "SECRET_MARKER")],
        false,
        "GHINVITE_STORAGE invalid",
    );
}
#[test]
fn bridge_mode_requires_valid_manifest_and_completed_readiness() {
    rejected(
        "missing",
        &[("GHINVITE_STORAGE", "d1-bridge")],
        false,
        "configuration missing",
    );
    let dir = std::env::temp_dir().join(format!("bridge-startup-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("authority.json");
    let token = "a".repeat(64);
    let values = [
        ("GHINVITE_STORAGE", "d1-bridge"),
        ("GHINVITE_D1_BRIDGE_URL", "https://127.0.0.1:1/v1/d1"),
        ("GHINVITE_D1_BRIDGE_TOKEN", token.as_str()),
        ("GHINVITE_D1_AUTHORITY_MANIFEST", path.to_str().unwrap()),
    ];
    rejected("missing-file", &values, false, "manifest unavailable");
    std::fs::write(&path, "SECRET_MARKER").unwrap();
    rejected("malformed", &values, false, "manifest invalid");
    let manifest = serde_json::json!({"protocol":1,"database_id":"00000000-0000-4000-8000-000000000001","migrations":[{"name":"0001_init.sql","sha256":"a".repeat(64)}],"schema_sha256":"b".repeat(64)});
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    rejected("readiness", &values, false, "D1 bridge unavailable");
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn non_unicode_storage_mode_never_defaults_to_sqlite() {
    use std::os::unix::ffi::OsStringExt;
    let mode = std::ffi::OsString::from_vec(b"SECRET_MARKER\xff".to_vec());
    let path = std::env::temp_dir().join(format!(
        "bridge-unicode-refused-{}.sqlite",
        std::process::id()
    ));
    assert!(!path.exists());
    rejected(
        "non-unicode",
        &[
            ("GHINVITE_STORAGE", mode.as_os_str()),
            ("GHINVITE_DATABASE_PATH", path.as_os_str()),
        ],
        false,
        "GHINVITE_STORAGE invalid",
    );
    assert!(!path.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn the_listener_check_tells_processes_apart() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    assert!(listens(std::process::id(), address));
    // PID 1 does not hold this test's socket.
    assert!(!listens(1, address));
    drop(listener);
    assert!(!listens(std::process::id(), address));
}
