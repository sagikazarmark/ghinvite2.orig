//! The `octoemu` binary: the startup line, seed files, the idle timeout,
//! `--fidelity` and argument errors.
#![cfg(not(target_arch = "wasm32"))]

use serde_json::Value;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn octoemu() -> Command {
    Command::new(env!("CARGO_BIN_EXE_octoemu"))
}

/// A running binary, killed when dropped.
struct Running {
    child: Child,
    ready: Value,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn(args: &[&str]) -> Running {
    let mut child = octoemu()
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.as_mut().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(line.ends_with('\n'), "one whole line: {line:?}");
    let ready = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"));
    Running { child, ready }
}

fn seed_file(name: &str, json: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&path, json).unwrap();
    path
}

#[tokio::test]
async fn prints_one_json_line_with_its_bases() {
    let mut running = spawn(&["--port", "0"]);
    let ready = running.ready.as_object().unwrap();
    let keys: Vec<&str> = ready.keys().map(String::as_str).collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(
        sorted,
        ["admin_base", "api_base", "pid", "port", "web_base"]
    );
    let port = ready["port"].as_u64().unwrap();
    assert_ne!(port, 0);
    assert_eq!(
        ready["pid"].as_u64().unwrap(),
        u64::from(running.child.id())
    );
    let root = format!("http://127.0.0.1:{port}");
    assert_eq!(ready["api_base"], root.as_str());
    assert_eq!(ready["web_base"], root.as_str());
    assert_eq!(ready["admin_base"], format!("{root}/__octoemu").as_str());

    let health = reqwest::get(format!("{root}/__octoemu/health"))
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    let unbuilt = reqwest::get(format!("{root}/orgs/acme/invitations"))
        .await
        .unwrap();
    assert_eq!(unbuilt.status(), 501);

    // Nothing else on stdout.
    running.child.kill().unwrap();
    let mut rest = String::new();
    running
        .child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut rest)
        .unwrap();
    assert_eq!(rest, "");
}

#[tokio::test]
async fn loads_a_seed_file() {
    let path = seed_file(
        "binary-seed.json",
        r#"{
            "settings": {"start_time": "2030-01-01T00:00:00Z"},
            "users": [{"id": 1, "login": "octocat"}],
            "orgs": [{"id": 2, "login": "acme", "plan": {"name": "team", "seats": 5}}]
        }"#,
    );
    let running = spawn(&["--seed", path.to_str().unwrap(), "--bind", "127.0.0.1"]);
    let admin = running.ready["admin_base"].as_str().unwrap();
    let response = reqwest::get(format!("{admin}/state")).await.unwrap();
    let date = response.headers()["date"].to_str().unwrap().to_owned();
    assert!(date.starts_with("Tue, 01 Jan 2030 00:00:"), "{date}");
    let state: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(state["world"]["orgs"][0]["plan"]["seats"], 5);
}

#[test]
fn exits_after_the_idle_timeout() {
    let mut running = spawn(&["--port", "0", "--idle-timeout", "1"]);
    let started = Instant::now();
    loop {
        if let Some(status) = running.child.try_wait().unwrap() {
            assert!(status.success(), "{status}");
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "still running after 15 s"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(started.elapsed() >= Duration::from_millis(900));
}

#[test]
fn rejects_an_invalid_seed() {
    let path = seed_file(
        "binary-bad-seed.json",
        r#"{"installations": [{"id": 1, "app_id": 2, "account": "ghost"}]}"#,
    );
    let output = octoemu()
        .args(["--seed", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("installations[0].app_id"), "{stderr}");
    assert!(stderr.contains("installations[0].account"), "{stderr}");

    let path = seed_file("binary-not-json.json", "{ users: ");
    let output = octoemu()
        .args(["--seed", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn prints_the_fidelity_table() {
    let output = octoemu().arg("--fidelity").output().unwrap();
    assert!(output.status.success());
    let table = String::from_utf8(output.stdout).unwrap();
    assert!(table.starts_with("KIND"), "{table}");
    assert!(table.contains("date-header"), "{table}");
    assert!(table.contains("documented"), "{table}");
    assert!(table.contains("assumed"), "{table}");
    assert!(table.contains("routes: 0"), "{table}");
}

#[test]
fn rejects_bad_arguments() {
    for args in [&["--port", "nope"][..], &["--wat"], &["--idle-timeout"]] {
        let output = octoemu().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty());
    }
    let help = octoemu().arg("--help").output().unwrap();
    assert!(help.status.success());
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("--idle-timeout")
    );
}
