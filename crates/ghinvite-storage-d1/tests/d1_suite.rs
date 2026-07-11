//! D1Storage smoke tests against wrangler dev --local.
//!
//! Run with:
//!   wrangler d1 migrations apply ghinvite --local --config wrangler/web.toml
//!   cargo test -p ghinvite-storage-d1 --features d1-suite -- --ignored

#![cfg(feature = "d1-suite")]

use std::process::Command;

fn wrangler_d1_execute(sql: &str) -> String {
    let out = Command::new("wrangler")
        .args([
            "d1",
            "execute",
            "ghinvite",
            "--local",
            "--config",
            "wrangler/web.toml",
            "--json",
            "--command",
            sql,
        ])
        .output()
        .expect("wrangler not on PATH — install with: npm i -g wrangler");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() || stdout.trim().is_empty() {
        return format!("[exit {}] stdout={stdout} stderr={stderr}", out.status);
    }
    stdout.to_string()
}

#[test]
#[ignore]
fn d1_schema_tables_exist() {
    for table in [
        "installations",
        "users",
        "invitation_links",
        "invitation_link_repos",
        "invitation_requests",
        "github_invitations",
        "audit_events",
    ] {
        let out = wrangler_d1_execute(&format!("SELECT COUNT(*) as cnt FROM \"{table}\""));
        assert!(
            out.contains("cnt"),
            "table {table} missing or inaccessible: {out}"
        );
    }
}
