//! D1Storage smoke tests against wrangler dev --local.
//!
//! Run with:
//!   wrangler d1 migrations apply ghinvite --local --config wrangler/web.toml
//!   cargo test -p ghinvite-storage-d1 --features d1-suite -- --ignored

#![cfg(feature = "d1-suite")]

use std::process::Command;

fn wrangler_d1_execute(sql: &str) -> String {
    let out = Command::new("wrangler")
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
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

/// Execute the production query builder in local D1, including historical UTC
/// encodings and nanosecond/ID boundaries. This tests D1 SQL, not merely Wasm compilation.
#[test]
#[ignore]
fn d1_audit_seek_queries_preserve_precision_and_use_indexes() {
    use ghinvite_core::{
        AuditEventId,
        audit::EventType,
        storage::{AuditBoundary, AuditPosition, audit_read},
    };
    let account = 939393;
    let mut sql = format!("DELETE FROM audit_events WHERE account_id IN ({account},939394);");
    for n in 0..53u128 {
        let id = AuditEventId::from_ulid(ulid::Ulid::from(n + 1));
        let time = match n {
            0 => "2026-05-04T12:00:00Z".to_string(),
            1 => "2026-05-04T12:00:00+00:00".to_string(),
            2 => "2026-05-04T12:00:00.000000001Z".to_string(),
            3 => "2026-05-04T12:00:00.100Z".to_string(),
            4 => "2026-05-04T12:00:00.100000+00:00".to_string(),
            _ => format!("2026-05-04T12:00:00.{:09}+00:00", 100000000 + n),
        };
        let scope = if n == 52 { 939394 } else { account };
        let event = if n == 51 {
            "request.declined"
        } else {
            "request.created"
        };
        sql.push_str(&format!("INSERT INTO audit_events (id,account_id,occurred_at,event_type,actor_kind,target_kind,target_id) VALUES ('{id}',{scope},'{time}','{event}','system','installation','row-{n}');"));
    }
    // Bind the generated SQL's placeholders with fixture-only literals, because
    // wrangler execute does not offer parameter binding. Production uses D1.bind.
    let query = |filter: Option<EventType>, position: AuditPosition, probe: bool| {
        let boundary = position.boundary();
        audit_read::query(filter, position, probe)
            .replace("?1", &account.to_string())
            .replace(
                "?2",
                &filter
                    .map(|e| format!("'{}'", e.as_str()))
                    .unwrap_or("NULL".into()),
            )
            .replace(
                "?3",
                &boundary
                    .map(|b| format!("'{}'", audit_read::boundary_time(b)))
                    .unwrap_or("NULL".into()),
            )
            .replace(
                "?4",
                &boundary
                    .map(|b| format!("'{}'", b.id))
                    .unwrap_or("NULL".into()),
            )
    };
    let boundary = |n: u128, time: &str| AuditBoundary {
        id: AuditEventId::from_ulid(ulid::Ulid::from(n + 1)),
        occurred_at: chrono::DateTime::parse_from_rfc3339(time)
            .unwrap()
            .with_timezone(&chrono::Utc),
    };
    let filtered = Some(EventType::RequestCreated);
    let cases = [
        (
            None,
            AuditPosition::Latest,
            (27..=51).rev().collect::<Vec<_>>(),
        ),
        (filtered, AuditPosition::Latest, (26..=50).rev().collect()),
        (
            filtered,
            AuditPosition::Before(boundary(26, "2026-05-04T12:00:00.100000026Z")),
            (1..=25).rev().collect(),
        ),
        (
            filtered,
            AuditPosition::Before(boundary(1, "2026-05-04T12:00:00Z")),
            vec![0],
        ),
        (
            filtered,
            AuditPosition::After(boundary(0, "2026-05-04T12:00:00Z")),
            (1..=25).collect(),
        ),
        (
            filtered,
            AuditPosition::After(boundary(25, "2026-05-04T12:00:00.100000025Z")),
            (26..=50).collect(),
        ),
        (
            filtered,
            AuditPosition::Before(boundary(0, "2026-05-04T12:00:00Z")),
            vec![],
        ),
    ];
    for (filter, position, _) in &cases {
        let q = query(*filter, *position, false);
        sql.push_str(&format!("{q}; EXPLAIN QUERY PLAN {q};"));
    }
    sql.push_str(&format!(
        "{};",
        query(
            filtered,
            AuditPosition::After(boundary(50, "2026-05-04T12:00:00.100000050Z")),
            true
        )
    ));
    sql.push_str(&format!(
        "DELETE FROM audit_events WHERE account_id IN ({account},939394);"
    ));
    let out = wrangler_d1_execute(&sql);
    let results: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|_| panic!("{out}"));
    let selects: Vec<_> = results
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["results"].as_array().is_some_and(|a| !a.is_empty()))
        .collect();
    let mut i = 0;
    for (filter, position, expected) in cases {
        if !expected.is_empty() {
            let rows: Vec<_> = selects[i]["results"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["target_id"].as_str().unwrap())
                .collect();
            let expected: Vec<_> = expected.into_iter().map(|n| format!("row-{n}")).collect();
            assert_eq!(rows, expected);
            i += 1;
        }
        let plan = selects[i]["results"].to_string();
        assert!(
            plan.contains(if filter.is_some() {
                "idx_audit_account_event_order"
            } else {
                "idx_audit_account_order"
            }),
            "{plan}"
        );
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
        if position != AuditPosition::Latest {
            assert!(plan.contains("<expr>"), "{plan}");
        }
        i += 1;
    }
    assert_eq!(
        i,
        selects.len(),
        "no phantom newer page at exact 25-row boundary"
    );
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
