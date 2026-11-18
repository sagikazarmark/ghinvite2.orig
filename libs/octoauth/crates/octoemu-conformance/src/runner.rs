//! Running the scenarios: compare or record snapshots, the schema check,
//! and the report.

use crate::ctx::{Ctx, Req, Stop, fail};
use crate::scenarios::{self, Scenario};
use crate::schema::Schemas;
use crate::snapshot::{Diff, Snapshot};
use crate::target::{Known, Target, TargetKind};
use serde::Serialize;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

/// Compare with the snapshots, or write them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Compare each run with its checked-in snapshot.
    Compare,
    /// Write each run as its snapshot. On the emulator, a snapshot
    /// recorded live is never overwritten.
    Record,
}

/// How to run.
#[derive(Clone, Debug)]
pub struct Options {
    /// Compare or record.
    pub mode: Mode,
    /// Live: run destructive scenarios too (they always run on the
    /// emulator).
    pub destructive: bool,
    /// Live: run slow scenarios too.
    pub slow: bool,
    /// Only these scenario IDs.
    pub only: Option<BTreeSet<String>>,
    /// Where the snapshots are.
    pub snapshots: PathBuf,
}

impl Options {
    /// From the environment: `OCTOEMU_CONFORMANCE` (`compare`, the default,
    /// or `record`), `OCTOEMU_CONFORMANCE_ONLY` (comma-separated IDs),
    /// `OCTOEMU_LIVE_DESTRUCTIVE=1`, `OCTOEMU_LIVE_SLOW=1`.
    pub fn from_env() -> Result<Self, String> {
        let flag = |name: &str| std::env::var(name).is_ok_and(|v| v == "1" || v == "true");
        let mode = match std::env::var("OCTOEMU_CONFORMANCE").as_deref() {
            Err(_) | Ok("" | "compare") => Mode::Compare,
            Ok("record") => Mode::Record,
            Ok(other) => return Err(format!("OCTOEMU_CONFORMANCE={other}: compare or record")),
        };
        let only = std::env::var("OCTOEMU_CONFORMANCE_ONLY")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(|v| v.split(',').map(|id| id.trim().to_owned()).collect());
        Ok(Self {
            mode,
            destructive: flag("OCTOEMU_LIVE_DESTRUCTIVE"),
            slow: flag("OCTOEMU_LIVE_SLOW"),
            only,
            snapshots: snapshot_dir(),
        })
    }
}

/// The checked-in snapshots.
pub fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("snapshots")
}

/// A scenario's result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// It matched its snapshot.
    Passed,
    /// Its snapshot was written.
    Recorded,
    /// It differs from its snapshot.
    Mismatch,
    /// It did not run.
    Skipped,
    /// It could not finish, or has no snapshot to compare with.
    Failed,
}

/// One scenario's report.
#[derive(Clone, Debug, Serialize)]
pub struct ScenarioReport {
    /// The scenario ID.
    pub id: String,
    /// The result.
    pub outcome: Outcome,
    /// Why it was skipped or failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Destructive on live.
    pub destructive: bool,
    /// Slow on live.
    pub slow: bool,
    /// The fidelity entries it measures.
    pub covers: Vec<String>,
    /// The compared snapshot's `source`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_source: Option<String>,
    /// Steps recorded.
    pub steps: usize,
    /// How the run differs from the snapshot.
    pub diff: Diff,
    /// Schema violations.
    pub schema_violations: Vec<String>,
    /// Responses the schema check could not fully check.
    pub schema_notes: Vec<String>,
}

/// A run's report.
#[derive(Clone, Debug, Serialize)]
pub struct Report {
    /// Where it ran.
    pub target: TargetKind,
    /// Compare or record.
    pub mode: Mode,
    /// The pinned description the schema check used.
    pub schema_source: serde_json::Value,
    /// Per scenario.
    pub scenarios: Vec<ScenarioReport>,
}

impl Report {
    /// What fails the run: a scenario that failed; a mismatch on the
    /// emulator, or on live against a live snapshot (against an emulator
    /// snapshot it is the fidelity signal, reported, not a failure); a
    /// schema violation on the emulator.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for scenario in &self.scenarios {
            let id = &scenario.id;
            match scenario.outcome {
                Outcome::Failed => problems.push(format!(
                    "{id}: failed: {}",
                    scenario.reason.as_deref().unwrap_or("")
                )),
                Outcome::Mismatch
                    if self.target == TargetKind::Emulator
                        || scenario.snapshot_source.as_deref()
                            == Some(crate::snapshot::LIVE_SOURCE) =>
                {
                    for mismatch in &scenario.diff.mismatches {
                        problems.push(format!("{id}: {mismatch}"));
                    }
                }
                _ => {}
            }
            if self.target == TargetKind::Emulator {
                for violation in &scenario.schema_violations {
                    problems.push(format!("{id}: schema: {violation}"));
                }
            }
        }
        problems
    }

    /// The report as text.
    pub fn text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "octoemu-conformance: target {:?}, mode {:?}, schemas {} @ {}",
            self.target,
            self.mode,
            self.schema_source["repository"].as_str().unwrap_or("?"),
            self.schema_source["commit"].as_str().unwrap_or("?"),
        );
        for s in &self.scenarios {
            let flags = match (s.destructive, s.slow) {
                (true, true) => " [destructive, slow]",
                (true, false) => " [destructive]",
                (false, true) => " [slow]",
                (false, false) => "",
            };
            let _ = write!(
                out,
                "  {:<28} {:<9} {} steps{flags}",
                s.id,
                format!("{:?}", s.outcome).to_lowercase(),
                s.steps
            );
            if let Some(source) = &s.snapshot_source {
                let _ = write!(out, ", snapshot: {source}");
            }
            if let Some(reason) = &s.reason {
                let _ = write!(out, " ({reason})");
            }
            out.push('\n');
            for (label, items) in [
                ("mismatch", &s.diff.mismatches),
                ("not in the response (unmodelled)", &s.diff.missing),
                ("not in the snapshot", &s.diff.extra),
                ("schema", &s.schema_violations),
                ("schema note", &s.schema_notes),
            ] {
                for item in items {
                    let _ = writeln!(out, "      {label}: {item}");
                }
            }
        }
        let count = |outcome: Outcome| {
            self.scenarios
                .iter()
                .filter(|s| s.outcome == outcome)
                .count()
        };
        let _ = writeln!(
            out,
            "{} scenarios: {} passed, {} recorded, {} mismatched, {} skipped, {} failed",
            self.scenarios.len(),
            count(Outcome::Passed),
            count(Outcome::Recorded),
            count(Outcome::Mismatch),
            count(Outcome::Skipped),
            count(Outcome::Failed)
        );
        out
    }
}

/// IDs and names the normaliser needs, through public routes.
async fn discover(ctx: &mut Ctx) -> Result<Known, Stop> {
    let world = ctx.world().clone();
    let jwt = ctx.app_jwt().await?;
    let app = ctx.call(Req::get("/app").bearer(&jwt)).await?;
    let Some(slug) = app.str("slug").map(str::to_owned) else {
        return fail(format!("GET /app: {} {}", app.status, app.body));
    };
    let token = ctx.token().await?;
    let id = |reply: crate::ctx::Reply, what: &str| match reply.u64("id") {
        Some(id) => Ok(id),
        None => fail(format!("{what}: {} {}", reply.status, reply.body)),
    };
    let org_id = id(
        ctx.call(Req::get(format!("/orgs/{}", world.org)).bearer(&token))
            .await?,
        "GET /orgs/{org}",
    )?;
    let repo_id = id(
        ctx.call(Req::get(format!("/repos/{}/{}", world.org, world.repo)).bearer(&token))
            .await?,
        "GET /repos/{owner}/{repo}",
    )?;
    let team_id = id(
        ctx.call(Req::get(format!("/orgs/{}/teams/{}", world.org, world.team)).bearer(&token))
            .await?,
        "GET /orgs/{org}/teams/{team_slug}",
    )?;
    let pat = world.invitee_token.clone();
    let invitee_id = id(
        ctx.call(Req::get(format!("/users/{}", world.invitee)).bearer(&pat))
            .await?,
        "GET /users/{username}",
    )?;
    let app_bot_id = ctx
        .call(Req::get(format!("/users/{slug}%5Bbot%5D")).bearer(&pat))
        .await?
        .u64("id");
    ctx.revoke(&token).await;
    Ok(Known {
        app_slug: slug,
        app_bot_id,
        org_id,
        repo_id,
        team_id,
        invitee_id,
    })
}

fn covers(id: &str) -> Vec<String> {
    octoemu::fidelity::Manifest::embedded()
        .ok()
        .and_then(|manifest| manifest.scenarios.get(id).cloned())
        .unwrap_or_default()
}

fn report_for(scenario: &Scenario, outcome: Outcome, reason: Option<String>) -> ScenarioReport {
    ScenarioReport {
        id: scenario.id.to_owned(),
        outcome,
        reason,
        destructive: scenario.destructive,
        slow: scenario.slow,
        covers: covers(scenario.id),
        snapshot_source: None,
        steps: 0,
        diff: Diff::default(),
        schema_violations: Vec::new(),
        schema_notes: Vec::new(),
    }
}

async fn run_one(
    target: &Target,
    options: &Options,
    schemas: &Arc<Schemas>,
    scenario: &Scenario,
) -> ScenarioReport {
    let live = target.kind() == TargetKind::Live;
    if live && scenario.destructive && !options.destructive {
        return report_for(
            scenario,
            Outcome::Skipped,
            Some("destructive: OCTOEMU_LIVE_DESTRUCTIVE=1 runs it, on a throwaway org".into()),
        );
    }
    if live && scenario.slow && !options.slow {
        return report_for(
            scenario,
            Outcome::Skipped,
            Some("live-only/slow: OCTOEMU_LIVE_SLOW=1 runs it".into()),
        );
    }
    let session = match target.session(scenario.setup).await {
        Ok(session) => session,
        Err(error) => return report_for(scenario, Outcome::Failed, Some(error)),
    };
    let emulator = session.emulator.map(Arc::new);
    let mut ctx = match Ctx::new(
        session.world,
        target.kind(),
        emulator.clone(),
        Some(schemas.clone()),
        options.slow,
    ) {
        Ok(ctx) => ctx,
        Err(error) => return report_for(scenario, Outcome::Failed, Some(error)),
    };
    let known = match target.cached_known() {
        Some(known) => Ok(known),
        None => discover(&mut ctx).await,
    };
    let result = match known {
        Ok(known) => {
            target.cache_known(&known);
            ctx.set_known(known);
            (scenario.run)(&mut ctx).await
        }
        Err(stop) => Err(stop),
    };
    let steps = std::mem::take(&mut ctx.steps);
    let mut report = report_for(scenario, Outcome::Passed, None);
    report.steps = steps.len();
    for (request, finding) in std::mem::take(&mut ctx.schema_findings) {
        report
            .schema_violations
            .extend(finding.violations.iter().map(|v| format!("{request}: {v}")));
        if let Some(note) = finding.note {
            report.schema_notes.push(note);
        }
    }
    report.schema_notes.sort();
    report.schema_notes.dedup();
    let incomplete = match result {
        Ok(()) => None,
        Err(Stop::Skip(reason)) => {
            report.outcome = Outcome::Skipped;
            report.reason = Some(reason);
            return report;
        }
        Err(Stop::Fail(reason)) => Some(reason),
    };
    let existing = match Snapshot::load(&options.snapshots, scenario.id) {
        Ok(existing) => existing,
        Err(error) => {
            report.outcome = Outcome::Failed;
            report.reason = Some(error);
            return report;
        }
    };
    let keep_live = !live && existing.as_ref().is_some_and(Snapshot::is_live);
    if options.mode == Mode::Record && !keep_live && (live || incomplete.is_none()) {
        let snapshot = Snapshot::new(scenario.id, live, steps, incomplete.clone());
        report.outcome = match snapshot.save(&options.snapshots) {
            Ok(()) => Outcome::Recorded,
            Err(error) => {
                report.reason = Some(error);
                Outcome::Failed
            }
        };
        if let Some(reason) = incomplete {
            report.outcome = Outcome::Failed;
            report.reason = Some(reason);
        }
        return report;
    }
    if let Some(reason) = incomplete {
        report.outcome = Outcome::Failed;
        report.reason = Some(reason);
        return report;
    }
    match existing {
        None => {
            report.outcome = Outcome::Failed;
            report.reason =
                Some("no snapshot: run with OCTOEMU_CONFORMANCE=record to write one".into());
        }
        Some(snapshot) => {
            report.snapshot_source = Some(snapshot.source.clone());
            report.diff = Diff::of(&snapshot.steps, &steps);
            if !report.diff.passes() {
                report.outcome = Outcome::Mismatch;
            }
            if keep_live && options.mode == Mode::Record {
                report.reason = Some("kept the live snapshot; compared instead".into());
            }
        }
    }
    report
}

/// Runs every scenario `options` selects against `target`.
pub async fn run(target: &Target, options: &Options) -> Result<Report, String> {
    let schemas = Arc::new(Schemas::embedded()?);
    let mut report = Report {
        target: target.kind(),
        mode: options.mode,
        schema_source: schemas.source().clone(),
        scenarios: Vec::new(),
    };
    let all = scenarios::all();
    if let Some(only) = &options.only {
        let known: BTreeSet<&str> = all.iter().map(|s| s.id).collect();
        let unknown: Vec<&String> = only
            .iter()
            .filter(|id| !known.contains(id.as_str()))
            .collect();
        if !unknown.is_empty() {
            return Err(format!("unknown scenario IDs: {unknown:?}"));
        }
    }
    for scenario in &all {
        if options
            .only
            .as_ref()
            .is_some_and(|only| !only.contains(scenario.id))
        {
            continue;
        }
        report
            .scenarios
            .push(run_one(target, options, &schemas, scenario).await);
    }
    Ok(report)
}

/// Writes the report as JSON to `OCTOEMU_CONFORMANCE_REPORT`, when set.
pub fn write_report(report: &Report) -> Result<(), String> {
    let Some(path) = std::env::var_os("OCTOEMU_CONFORMANCE_REPORT") else {
        return Ok(());
    };
    let text = serde_json::to_string_pretty(report).map_err(|error| error.to_string())?;
    std::fs::write(&path, text).map_err(|error| format!("{}: {error}", path.to_string_lossy()))
}
