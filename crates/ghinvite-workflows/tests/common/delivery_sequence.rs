//! AC-DLV-3 generated response sequences and an independent trace oracle.
//! The oracle does not call production confirmed(), transition or settlement reducers.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireReply {
    Created,
    NoContent,
    Unauthorized,
    Forbidden,
    NotFound,
    Validation,
    AlreadyExists,
    Spammed,
    Policy,
    Timeout,
    PrimaryLimit,
    SecondaryLimit,
}
pub const ALPHABET: [WireReply; 12] = [
    WireReply::Created,
    WireReply::NoContent,
    WireReply::Unauthorized,
    WireReply::Forbidden,
    WireReply::NotFound,
    WireReply::Validation,
    WireReply::AlreadyExists,
    WireReply::Spammed,
    WireReply::Policy,
    WireReply::Timeout,
    WireReply::PrimaryLimit,
    WireReply::SecondaryLimit,
];
impl WireReply {
    pub fn status(self) -> u16 {
        match self {
            Self::Created => 201,
            Self::NoContent => 204,
            Self::Unauthorized => 401,
            Self::Forbidden => 403,
            Self::NotFound => 404,
            Self::Validation | Self::AlreadyExists | Self::Spammed | Self::Policy => 422,
            Self::Timeout => 503,
            Self::PrimaryLimit | Self::SecondaryLimit => 429,
        }
    }
    pub fn body(self) -> Value {
        match self {
            Self::AlreadyExists => {
                json!({"message":"Validation Failed","errors":[{"resource":"Invitation","code":"already_exists","field":"invitee_id"}]})
            }
            Self::Spammed => json!({"errors":[{"code":"spammed"}]}),
            Self::Policy => {
                json!({"message":"Inviting outside collaborators is restricted by organization policy."})
            }
            Self::Validation => {
                json!({"message":"Validation Failed","errors":[{"resource":"Invitation","code":"unprocessable","field":"invitee_id"}]})
            }
            Self::PrimaryLimit | Self::SecondaryLimit => json!({"message":"synthetic rate limit"}),
            _ => json!({"message":"synthetic response sequence"}),
        }
    }
    pub fn successful(self) -> bool {
        matches!(self, Self::Created | Self::NoContent)
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Create,
    Replay,
    Status,
    Present,
    Absent,
    Accepted,
    Expired,
    WaitDeadline,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Plan {
    pub seed: u64,
    pub replies: Vec<WireReply>,
    pub actions: Vec<Action>,
    pub lose_run_acks: bool,
}
struct Random(u64);
impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut n = self.0;
        n = (n ^ (n >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        n = (n ^ (n >> 27)).wrapping_mul(0x94d049bb133111eb);
        n ^ (n >> 31)
    }
}
impl Plan {
    pub fn generate(seed: u64) -> Self {
        let mut random = Random(seed);
        // Each residue forces one alphabet member to be the first physical HTTP
        // write, rather than merely generating unreachable suffix responses.
        let first = ALPHABET[(seed % ALPHABET.len() as u64) as usize];
        let mut replies = vec![first];
        for _ in 0..5 {
            replies.push(ALPHABET[(random.next() % ALPHABET.len() as u64) as usize]);
        }
        replies.push(WireReply::Created);
        let choices = [
            Action::Create,
            Action::Replay,
            Action::Status,
            Action::Present,
            Action::Absent,
            Action::Accepted,
            Action::Expired,
            Action::WaitDeadline,
        ];
        let mut actions = vec![
            Action::Create,
            Action::WaitDeadline,
            Action::Create,
            Action::Status,
        ];
        for _ in 0..14 {
            actions.push(choices[(random.next() % choices.len() as u64) as usize]);
        }
        actions.extend([
            Action::WaitDeadline,
            Action::Create,
            Action::Present,
            Action::Absent,
            Action::Accepted,
            Action::Expired,
            Action::Replay,
            Action::Status,
        ]);
        Self {
            seed,
            replies,
            actions,
            lose_run_acks: first == WireReply::Created && (seed / 12) % 2 == 1,
        }
    }
}
pub fn plans() -> Vec<Plan> {
    if let Ok(seed) = std::env::var("GHINVITE_DELIVERY_PROPERTY_SEED") {
        return vec![Plan::generate(
            seed.parse().expect("unsigned reproducible seed"),
        )];
    }
    let count = std::env::var("GHINVITE_DELIVERY_PROPERTY_CASES")
        .map(|v| v.parse::<usize>().expect("property case count"))
        .unwrap_or(24);
    assert!(
        (12..=96).contains(&count),
        "full corpus requires 12–96 cases; use explicit seed for one reproduction"
    );
    (0..count as u64).map(Plan::generate).collect()
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sample {
    pub snapshot: Value,
    pub mutations: BTreeMap<String, usize>,
    pub consumed: Vec<WireReply>,
    pub audit_settlements: Vec<Value>,
    pub counters: Value,
}
#[derive(Clone, Default)]
pub struct Oracle {
    confirmed: BTreeMap<String, Value>,
    settled: BTreeMap<String, Value>,
    frozen_writes: BTreeMap<String, usize>,
    last_revision: u64,
}
fn records(snapshot: &Value) -> BTreeMap<String, (&Value, &Value)> {
    let mut values = BTreeMap::from([(
        "root".into(),
        (&snapshot["create"], &snapshot["settlement"]),
    )]);
    if let Some(teams) = snapshot.get("teams").and_then(Value::as_object) {
        for (id, team) in teams {
            values.insert(format!("team:{id}"), (&team["create"], &team["settlement"]));
        }
    }
    values
}
// Compute the expected audit identities independently from immutable command
// fields; do not call production event-ID/settlement constructors.
fn audit_targets(snapshot: &Value) -> BTreeMap<String, (&str, &Value)> {
    let command = &snapshot["create"]["command"];
    let organization = snapshot.get("teams").is_some();
    let root = if organization {
        format!(
            "org:{}:{}:{}",
            command["request_id"].as_str().unwrap(),
            command["organization"]["org_id"].as_u64().unwrap(),
            command["delivery_generation"].as_u64().unwrap()
        )
    } else {
        command["invitation_id"].as_str().unwrap().to_owned()
    };
    let mut targets = BTreeMap::from([(
        root.clone(),
        (
            if organization {
                "organization_delivery"
            } else {
                "github_invitation"
            },
            snapshot,
        ),
    )]);
    if let Some(teams) = snapshot.get("teams").and_then(Value::as_object) {
        for (id, team) in teams {
            targets.insert(format!("{root}/team:{id}"), ("team_delivery", team));
        }
    }
    targets
}
fn check_audits(sample: &Sample, require_complete: bool) -> Result<(), String> {
    let targets = audit_targets(&sample.snapshot);
    let mut counts = BTreeMap::<&str, usize>::new();
    for event in &sample.audit_settlements {
        let id = event["target_id"]
            .as_str()
            .ok_or("audit missing target identity")?;
        let (kind, record) = targets
            .get(id)
            .ok_or("settlement audit for foreign target")?;
        let settlement = &record["settlement"];
        if event["target_kind"] != *kind || settlement.is_null() {
            return Err("settlement audit without matching retained target settlement".into());
        }
        let count = counts.entry(id).or_default();
        *count += 1;
        if *count > 1 {
            return Err(format!("multiple durable settlement audit facts for {id}"));
        }
        if *kind != "team_delivery" && event["id"] != settlement["event"]["id"] {
            return Err("root audit/owner event identities differ".into());
        }
        let state = settlement["state"]
            .as_str()
            .ok_or("settlement missing state")?;
        let expected = if state == "accepted" && *kind == "organization_delivery" {
            "org_invitation.accepted".to_owned()
        } else if state == "accepted"
            && *kind == "team_delivery"
            && record["create"]["outcome"]["via"]["kind"] == "organization_invitation"
        {
            "team_membership.added".to_owned()
        } else {
            format!("invitation.{state}")
        };
        if event["event_type"] != expected {
            return Err("audit event disagrees with target settlement state".into());
        }
    }
    if require_complete
        && targets.iter().any(|(id, (_, record))| {
            !record["settlement"].is_null()
                && counts.get(id.as_str()).copied().unwrap_or_default() != 1
        })
    {
        return Err("retained target settlement missing its durable audit fact".into());
    }
    Ok(())
}
pub fn has_settlement(snapshot: &Value) -> bool {
    records(snapshot)
        .values()
        .any(|(_, settlement)| !settlement.is_null())
}
pub fn audits_complete(sample: &Sample) -> bool {
    check_audits(sample, true).is_ok()
}
fn confirmed(kind: &str) -> bool {
    matches!(
        kind,
        "created"
            | "already_pending"
            | "already_satisfied"
            | "upgraded"
            | "skipped_custom_role"
            | "aborted"
            | "failed"
    )
}
impl Oracle {
    pub fn check(&mut self, sample: &Sample) -> Result<(), String> {
        let revision = sample.snapshot["revision"]
            .as_u64()
            .ok_or("missing owner revision")?;
        if revision < self.last_revision {
            return Err("owner revision went backwards".into());
        }
        self.last_revision = revision;
        for (target, (receipt, settlement)) in records(&sample.snapshot) {
            let outcome = receipt
                .get("outcome")
                .and_then(Value::as_object)
                .ok_or("missing single create outcome object")?;
            let kind = outcome
                .get("kind")
                .and_then(Value::as_str)
                .ok_or("missing create outcome kind")?;
            if !matches!(
                kind,
                "planned"
                    | "held"
                    | "submitted"
                    | "blocked"
                    | "throttled"
                    | "outcome_unknown"
                    | "created"
                    | "already_pending"
                    | "already_satisfied"
                    | "upgraded"
                    | "skipped_custom_role"
                    | "aborted"
                    | "failed"
            ) {
                return Err("unknown create outcome".into());
            }
            if let Some(first) = self.confirmed.get(&target) {
                if first != receipt {
                    return Err(format!("confirmed create receipt replaced for {target}"));
                }
                if sample.mutations.get(&target).copied().unwrap_or_default()
                    != self.frozen_writes[&target]
                {
                    return Err(format!("mutation repeated after confirmation for {target}"));
                }
            } else if confirmed(kind) {
                self.confirmed.insert(target.clone(), receipt.clone());
                self.frozen_writes.insert(
                    target.clone(),
                    sample.mutations.get(&target).copied().unwrap_or_default(),
                );
            }
            if let Some(first) = self.settled.get(&target) {
                if first != settlement {
                    return Err(format!("first settlement replaced or removed for {target}"));
                }
            } else if !settlement.is_null() {
                if !matches!(kind, "created" | "already_pending") {
                    return Err(format!(
                        "settlement without tracked invitation for {target}"
                    ));
                }
                if !matches!(
                    settlement["state"].as_str(),
                    Some("accepted" | "declined" | "expired" | "cancelled")
                ) {
                    return Err("invalid terminal settlement".into());
                }
                self.settled.insert(target, settlement.clone());
            }
        }
        if sample.consumed.iter().filter(|r| r.successful()).count() > 1 {
            return Err(
                "multiple successful create HTTP responses in one delivery generation".into(),
            );
        }
        check_audits(sample, false)?;
        if let Some(budget) = sample.counters.get("budget") {
            let lower = sample
                .consumed
                .iter()
                .filter(|r| r.successful() || **r == WireReply::Timeout)
                .count() as u64;
            if budget["content_used_hour"]
                .as_u64()
                .ok_or("missing actual budget charge")?
                < lower
            {
                return Err("accepted or uncertain physical write lost its budget charge".into());
            }
            let spammed = sample
                .consumed
                .iter()
                .filter(|r| **r == WireReply::Spammed)
                .count() as u64;
            let limited = sample
                .consumed
                .iter()
                .filter(|r| matches!(r, WireReply::PrimaryLimit | WireReply::SecondaryLimit))
                .count() as u64;
            if budget["limit_counts"]["spammed"].as_u64() != Some(spammed)
                || budget["limit_counts"]["rate_limited"].as_u64() != Some(limited)
            {
                return Err("limit observation count differs from actual HTTP replies".into());
            }
        }
        Ok(())
    }
    pub fn controls(&self, sample: &Sample) -> Vec<&'static str> {
        let mut ran = vec![];
        let mut bad = sample.clone();
        bad.snapshot["create"]["outcome"] = Value::Null;
        assert!(self.clone().check(&bad).is_err());
        ran.push("missing_outcome");
        if self.confirmed.contains_key("root") {
            let mut bad = sample.clone();
            bad.snapshot["create"]["outcome"] =
                json!({"kind":"failed","reason":{"kind":"synthetic_control"}});
            if bad.snapshot["create"] != sample.snapshot["create"] {
                assert!(self.clone().check(&bad).is_err());
                ran.push("replaced_confirmed_receipt");
            }
            let mut bad = sample.clone();
            *bad.mutations.entry("root".into()).or_default() += 1;
            assert!(self.clone().check(&bad).is_err());
            ran.push("duplicate_physical_write");
        }
        if !sample.snapshot["settlement"].is_null() {
            let mut bad = sample.clone();
            bad.snapshot["settlement"]["state"] =
                json!(if sample.snapshot["settlement"]["state"] == "accepted" {
                    "declined"
                } else {
                    "accepted"
                });
            assert!(self.clone().check(&bad).is_err());
            ran.push("competing_settlement");
            let mut bad = sample.clone();
            let event = sample
                .audit_settlements
                .first()
                .cloned()
                .unwrap_or_else(|| sample.snapshot["settlement"]["event"].clone());
            bad.audit_settlements = vec![event.clone(), event];
            assert!(self.clone().check(&bad).is_err());
            ran.push("duplicate_durable_settlement");
        }
        if let Some(event) = sample
            .audit_settlements
            .iter()
            .find(|e| e["target_kind"] == "team_delivery")
        {
            let mut bad = sample.clone();
            bad.audit_settlements.push(event.clone());
            assert!(self.clone().check(&bad).is_err());
            ran.push("duplicate_team_durable_settlement");
        }
        if sample.counters.get("budget").is_some()
            && sample
                .consumed
                .iter()
                .any(|r| r.successful() || *r == WireReply::Timeout)
        {
            let mut bad = sample.clone();
            bad.counters["budget"]["content_used_hour"] = json!(0);
            assert!(self.clone().check(&bad).is_err());
            ran.push("undercharged_uncertain_or_accepted_write");
        }
        ran
    }
}
pub struct Evidence {
    pub directory: PathBuf,
    plan: Plan,
    steps: Vec<Value>,
    controls: BTreeSet<String>,
}
impl Evidence {
    pub fn new(kind: &str, plan: Plan) -> Self {
        let root = std::env::var_os("GHINVITE_DELIVERY_PROPERTY_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::temp_dir()
                    .join(format!("ghinvite-delivery-property-{}", std::process::id()))
            });
        let directory = root.join(kind).join(format!("seed-{:016x}", plan.seed));
        std::fs::create_dir_all(&directory).unwrap();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join("plan.json"))
            .expect("fresh evidence directory required");
        serde_json::to_writer_pretty(file, &plan).unwrap();
        Self {
            directory,
            plan,
            steps: vec![],
            controls: BTreeSet::new(),
        }
    }
    pub fn before(&self, index: usize, action: Action) {
        std::fs::write(self.directory.join("checkpoint.json"),serde_json::to_vec_pretty(&json!({"seed":self.plan.seed,"step":index,"action":action,"state":"running","completed_steps":self.steps.len()})).unwrap()).unwrap();
    }
    pub fn after(&mut self, index: usize, action: Action, sample: &Sample, controls: Vec<&str>) {
        self.controls
            .extend(controls.into_iter().map(str::to_owned));
        let value = json!({"step":index,"action":action,"sample":sample});
        if self.steps.last().is_some_and(|s| s["step"] == index) {
            *self.steps.last_mut().unwrap() = value;
        } else {
            self.steps.push(value);
        }
        self.save("running", None);
    }
    fn save(&self, status: &str, failure: Option<&str>) {
        std::fs::write(self.directory.join("results.json"),serde_json::to_vec_pretty(&json!({"schema":1,"scope":"Actual native Restate owners and synthetic GitHub HTTP sequences; not live provider or release acceptance.","seed":self.plan.seed,"status":status,"plan":self.plan,"steps":self.steps,"controls":self.controls,"failure":failure,"counterexample_scope":"Full generated plan plus observed prefix through first detected failure; no claim of globally minimal trace."})).unwrap()).unwrap();
    }
    pub fn fail(&self, message: &str) -> ! {
        self.save("failed", Some(message));
        panic!(
            "AC-DLV-3 seed={} evidence={} failure={message}",
            self.plan.seed,
            self.directory.display()
        );
    }
    pub fn finish(&self) {
        self.save("passed", None);
    }
}
pub async fn settlement_facts(storage: &impl ghinvite_core::storage::ConsoleStorage) -> Vec<Value> {
    use ghinvite_core::{audit::EventType, storage::audit_read::AuditPosition};
    let mut events = vec![];
    for kind in [
        EventType::InvitationAccepted,
        EventType::OrgInvitationAccepted,
        EventType::TeamMembershipAdded,
        EventType::InvitationDeclined,
        EventType::InvitationExpired,
        EventType::InvitationCancelled,
    ] {
        let page = storage
            .list_audit_events(100, Some(kind), AuditPosition::Latest)
            .await
            .unwrap();
        assert!(
            !page.has_older,
            "isolated property case must inspect the complete audit set"
        );
        events.extend(
            page.events
                .into_iter()
                // Canonical team creation also uses this type with a system
                // actor. Its GitHub-actor form records accepted settlement.
                .filter(|e| {
                    kind != EventType::TeamMembershipAdded
                        || e.actor_kind == ghinvite_core::audit::ActorKind::Github
                })
                .map(|e| serde_json::to_value(e).unwrap()),
        );
    }
    events
}
pub async fn wait_deadline(snapshot: &Value) {
    if snapshot["create"]["outcome"]["kind"] != "throttled" {
        return;
    }
    let until: chrono::DateTime<chrono::Utc> =
        serde_json::from_value(snapshot["create"]["outcome"]["until"].clone()).unwrap();
    let delay = (until - chrono::Utc::now()).to_std().unwrap_or_default();
    // Keep original long closures intact. Their immediate replays must remain
    // refused, rather than rewriting clocks or waiting a day in a test.
    if delay <= std::time::Duration::from_secs(2) {
        tokio::time::sleep(delay + std::time::Duration::from_millis(30)).await;
    }
}
pub fn require_coverage(
    consumed: &BTreeSet<WireReply>,
    controls: &BTreeSet<String>,
    settled: usize,
    multi_response: usize,
) {
    if std::env::var_os("GHINVITE_DELIVERY_PROPERTY_SEED").is_some() {
        return;
    }
    assert_eq!(
        *consumed,
        ALPHABET.into_iter().collect(),
        "every required response must actually cross the HTTP write seam"
    );
    for name in [
        "missing_outcome",
        "replaced_confirmed_receipt",
        "duplicate_physical_write",
        "competing_settlement",
        "duplicate_durable_settlement",
    ] {
        assert!(
            controls.contains(name),
            "oracle control did not execute: {name}"
        );
    }
    assert!(
        settled > 0,
        "at-most-one settlement assertion must encounter real settlements"
    );
    assert!(
        multi_response > 0,
        "generated cases must consume multiple real response symbols, not just singleton fixed examples"
    );
}

pub fn write_summary(
    kind: &str,
    seeds: &[u64],
    consumed: &BTreeSet<WireReply>,
    controls: &BTreeSet<String>,
    settled: usize,
    multi_response: usize,
) {
    let root = std::env::var_os("GHINVITE_DELIVERY_PROPERTY_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("ghinvite-delivery-property-{}", std::process::id()))
        });
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join(kind).join("summary.json"))
        .expect("fresh summary path required");
    serde_json::to_writer_pretty(file,&json!({"schema":1,"kind":kind,"status":"passed","seeds":seeds,"cases":seeds.len(),"consumed_response_alphabet":consumed,"controls":controls,"settled_cases":settled,"multi_response_cases":multi_response,"scope":"Generated native real-owner acceptance only; no live provider or deployed gate claim."})).unwrap();
}
