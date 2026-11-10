//! SD-04: real durable ingress ordering while 500 admissions contend for one owner.
use super::*;
use sha2::{Digest, Sha256};
use tokio::task::JoinSet;

const SECRET: &str = "SD04_PRIVATE_LINK_SECRET_MARKER_NOT_AN_INGRESS_VALUE";
const ADMISSIONS: usize = 500;
const BEFORE: usize = 250;

async fn enqueue(r: &Runtime, link: InvitationLinkId, handler: &str, body: &Value) -> String {
    let response = r
        .client
        .post(format!(
            "{}/InvitationLink/{link}/{handler}/send",
            r.ingress
        ))
        .json(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    response.json::<Value>().await.unwrap()["invocationId"]
        .as_str()
        .unwrap()
        .to_owned()
}
async fn attach(client: reqwest::Client, ingress: String, id: String) -> Value {
    let response = client
        .get(format!("{ingress}/restate/invocation/{id}/attach"))
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, 200, "completed invocation: {body}");
    serde_json::from_str(&body).unwrap()
}
async fn cohort(r: &Runtime, link: InvitationLinkId, inputs: &[Value]) -> Vec<String> {
    let mut tasks = JoinSet::new();
    for input in inputs {
        let client = r.client.clone();
        let url = format!("{}/InvitationLink/{link}/admit/send", r.ingress);
        let input = input.clone();
        tasks.spawn(async move {
            let response = client.post(url).json(&input).send().await.unwrap();
            assert_eq!(response.status(), 202);
            response.json::<Value>().await.unwrap()["invocationId"]
                .as_str()
                .unwrap()
                .to_owned()
        });
    }
    let mut ids = vec![];
    while let Some(result) = tasks.join_next().await {
        ids.push(result.unwrap());
    }
    ids
}
async fn receipts(r: &Runtime, ids: &[String]) -> Vec<Value> {
    let mut tasks = JoinSet::new();
    for id in ids {
        tasks.spawn(attach(r.client.clone(), r.ingress.clone(), id.clone()));
    }
    let mut results = vec![];
    while let Some(result) = tasks.join_next().await {
        results.push(result.unwrap());
    }
    results
}
async fn privacy(r: &Runtime) {
    let admin = r.harness.restate.admin();
    let rows = admin
        .sql_or_panic("SELECT raw FROM sys_journal LIMIT 100001")
        .await;
    assert!(
        !rows.is_empty() && rows.len() <= 100000,
        "complete bounded journal scan"
    );
    let mut bytes_scanned = 0;
    for row in &rows {
        let hex = row["raw"].as_str().unwrap();
        assert!(hex.len().is_multiple_of(2));
        let bytes: Vec<u8> = hex
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        bytes_scanned += bytes.len();
        assert!(
            bytes_scanned <= 64 * 1024 * 1024,
            "journal byte bound exceeded"
        );
        assert!(!bytes.windows(SECRET.len()).any(|b| b == SECRET.as_bytes()));
    }
    let states = admin.sql_or_panic("SELECT * FROM state LIMIT 20001").await;
    assert!(
        !states.is_empty() && states.len() <= 20000,
        "complete bounded state scan"
    );
    let encoded = serde_json::to_string(&states).unwrap();
    assert!(encoded.len() <= 64 * 1024 * 1024);
    assert!(!encoded.contains(SECRET));
    let secret_hex: String = SECRET.bytes().map(|b| format!("{b:02x}")).collect();
    assert!(!encoded.contains(&secret_hex));
    println!(
        "SD04 scanned {} raw journals ({bytes_scanned} bytes), {} state rows",
        rows.len(),
        states.len()
    );
}
async fn scenario(handler: &str, max: u32, expected_accepted: usize) {
    let r = Runtime::start().await;
    for user_id in 1000..1000 + ADMISSIONS as u64 {
        r.storage
            .upsert_user(&ghinvite_core::User {
                user_id,
                login: format!("load{user_id}"),
                avatar_url: None,
                github_created_at: None,
                last_seen_at: chrono::Utc::now(),
            })
            .await
            .unwrap();
    }
    ghinvite_e2e::journal::JournalAudit::retain(
        &r.harness,
        &[
            "InvitationLink",
            "InvitationRequest",
            "InvitationProjection",
        ],
    )
    .await;
    let link = InvitationLinkId::new();
    let hash: String = Sha256::digest(SECRET.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let create = json!({"link_id":link,"secret_sha256":hash,"account_id":100,"installation_id":1,
        "admin":{"account_id":100,"user_id":7},"description":"SD04 admission ordering",
        "approval_required":true,"max_uses":max,"abuse_threshold_per_hour":1000,
        "targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]}});
    assert!(
        r.ok("InvitationLink", link, "create", &create)
            .await
            .get("Ok")
            .is_some()
    );
    let inputs: Vec<Value> = (0..ADMISSIONS)
        .map(|i| {
            let mut input = attempt(link, 1000 + i as u64);
            input["secret_sha256"] = json!(hash);
            input
        })
        .collect();
    // Hold the first owner invocation before its decision. Every subsequent
    // send acknowledgement proves durable ingress acceptance, not client spawn order.
    r.link_faults.arm("before-decision");
    let first = enqueue(&r, link, "admit", &inputs[0]).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while r.link_faults.attempts() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("first admission occupies exclusive owner");
    let mut before = cohort(&r, link, &inputs[1..BEFORE]).await;
    before.push(first);
    let lifecycle = if handler == "pause" {
        json!({"link_id":link,"admin":{"account_id":100,"user_id":7},"operation_id":RequestId::new()})
    } else {
        revoke_command(link)
    };
    let lifecycle_id = enqueue(&r, link, handler, &lifecycle).await;
    let after = cohort(&r, link, &inputs[BEFORE..]).await;
    assert_eq!(before.len() + after.len(), ADMISSIONS);
    // All 500 admissions and the control are durably queued while the first
    // invocation is still blocked. Release only after the final acknowledgement.
    assert!(r.link_faults.attempts() > 0);
    let queued = r.harness.restate.admin().sql_or_panic(&format!(
        "SELECT target_handler_name,status,count(*) AS n FROM sys_invocation WHERE target_service_name='InvitationLink' AND target_service_key='{link}' GROUP BY target_handler_name,status"
    )).await;
    let mut admitted_pending = 0;
    let mut control_pending = 0;
    for row in queued {
        if row["target_handler_name"] == "admit" {
            assert!(matches!(
                row["status"].as_str(),
                Some("pending" | "running" | "backing-off")
            ));
            admitted_pending += row["n"].as_u64().unwrap();
        } else if row["target_handler_name"] == handler {
            assert_eq!(row["status"], "pending");
            control_pending += row["n"].as_u64().unwrap();
        }
    }
    assert_eq!(admitted_pending, ADMISSIONS as u64);
    assert_eq!(control_pending, 1);
    r.link_faults.release();
    let (before_results, after_results, control) =
        tokio::time::timeout(Duration::from_secs(360), async {
            tokio::join!(
                receipts(&r, &before),
                receipts(&r, &after),
                attach(r.client.clone(), r.ingress.clone(), lifecycle_id)
            )
        })
        .await
        .expect("500 admissions and lifecycle finish within the load gate");
    assert!(control.get("Ok").is_some(), "{control}");
    assert_eq!(
        before_results
            .iter()
            .filter(|v| v["result"]["kind"] == "accepted")
            .count(),
        expected_accepted
    );
    assert!(
        after_results
            .iter()
            .all(|v| v["result"]["kind"] == "rejected")
    );
    let refusal = if handler == "pause" && expected_accepted < max as usize {
        "paused"
    } else {
        "not_available"
    };
    let mut reasons = std::collections::BTreeMap::new();
    for result in &after_results {
        *reasons
            .entry(result["result"]["reason"].to_string())
            .or_insert(0usize) += 1;
    }
    println!("SD04 {handler} max={max}: accepted={expected_accepted}, post-control={reasons:?}");
    assert!(
        after_results
            .iter()
            .all(|v| v["result"]["reason"] == refusal),
        "unexpected refusals: {reasons:?}"
    );
    assert_eq!(r.uses(link).await, expected_accepted);
    assert!(expected_accepted <= max as usize);
    // Exact retries preserve both an accepted and a post-control rejected result.
    let accepted = r.ok("InvitationLink", link, "admit", &inputs[0]).await;
    assert_eq!(accepted["result"]["kind"], "accepted");
    assert_eq!(
        r.ok("InvitationLink", link, "admit", &inputs[0]).await,
        accepted
    );
    let rejected = r
        .ok("InvitationLink", link, "admit", &inputs[ADMISSIONS - 1])
        .await;
    assert_eq!(rejected["result"]["reason"], refusal);
    assert_eq!(
        r.ok("InvitationLink", link, handler, &lifecycle).await,
        control
    );
    assert_eq!(r.uses(link).await, expected_accepted);
    privacy(&r).await;
    r.harness.finish().await;
}
#[tokio::test]
async fn five_hundred_concurrent_admissions_obey_pause_with_capacity_remaining() {
    scenario("pause", 400, BEFORE).await;
}
#[tokio::test]
async fn five_hundred_concurrent_admissions_never_exceed_max_uses() {
    scenario("pause", 100, 100).await;
}
#[tokio::test]
async fn five_hundred_concurrent_admissions_obey_revocation_order_and_receipt_replay() {
    scenario("revoke", 1000, BEFORE).await;
}
