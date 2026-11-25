//! Checked-in snapshots, LocalStack style: one JSON file per scenario under
//! `snapshots/`, with each recorded step's request line, status, relied-on
//! headers and normalised body.
//!
//! A live run records them (`source: "live"`). Until the first live run,
//! they are generated from the emulator and say so
//! (`source: "emulator (unverified)"`): the first live recording overwrites
//! them, and any diff it shows is the fidelity signal.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `source` of a snapshot generated from the emulator.
pub const EMULATOR_SOURCE: &str = "emulator (unverified)";
/// `source` of a snapshot recorded against GitHub.
pub const LIVE_SOURCE: &str = "live";
/// `source` of a snapshot recorded by the live target pointed at another
/// endpoint (`OCTOEMU_LIVE_API_BASE`): a smoke test, never a baseline.
pub const CUSTOM_SOURCE: &str = "custom endpoint (not GitHub)";

/// Who answered a recorded run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provenance {
    /// The embedded emulator.
    Emulator,
    /// GitHub itself.
    GitHub,
    /// The live target at an endpoint that is not GitHub's.
    Custom,
}

const CUSTOM_NOTE: &str = "Recorded by the live target against an endpoint that is not GitHub's: \
a smoke test, not a baseline. Record against GitHub to replace it.";

const EMULATOR_NOTE: &str = "Generated from octoemu, not GitHub: unverified. \
The first live run (record mode) overwrites this file; any diff it shows is the fidelity signal.";

/// One recorded step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// The step's name, unique within the scenario.
    pub step: String,
    /// The normalised request line (`PUT /repos/<org>/<repo>/…`); absent for
    /// an observation (a fact derived from responses).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
    /// The response status; absent for an observation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// The relied-on headers present, normalised.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// The normalised body: JSON, `{"$form": {…}}` for a form-encoded one,
    /// `{"$text": "…"}` for other text, `null` for none.
    pub body: Value,
}

/// A scenario's snapshot file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    /// The scenario ID.
    pub scenario: String,
    /// [`LIVE_SOURCE`] or [`EMULATOR_SOURCE`].
    pub source: String,
    /// The day a live run recorded it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded: Option<String>,
    /// What this file is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Why the recording run stopped early, if it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete: Option<String>,
    /// The steps, in order.
    pub steps: Vec<Step>,
}

impl Snapshot {
    /// A snapshot of a run `provenance` answered.
    pub fn new(
        scenario: &str,
        provenance: Provenance,
        steps: Vec<Step>,
        incomplete: Option<String>,
    ) -> Self {
        let (source, note) = match provenance {
            Provenance::Emulator => (EMULATOR_SOURCE, Some(EMULATOR_NOTE)),
            Provenance::GitHub => (LIVE_SOURCE, None),
            Provenance::Custom => (CUSTOM_SOURCE, Some(CUSTOM_NOTE)),
        };
        Self {
            scenario: scenario.to_owned(),
            source: source.to_owned(),
            recorded: (provenance != Provenance::Emulator)
                .then(|| chrono::Utc::now().format("%Y-%m-%d").to_string()),
            note: note.map(str::to_owned),
            incomplete,
            steps,
        }
    }

    /// Whether GitHub produced it.
    pub fn is_live(&self) -> bool {
        self.source == LIVE_SOURCE
    }

    /// The file for `scenario` under `dir`.
    pub fn path(dir: &Path, scenario: &str) -> PathBuf {
        dir.join(format!("{scenario}.json"))
    }

    /// Reads `scenario`'s snapshot; `Ok(None)` when there is none.
    pub fn load(dir: &Path, scenario: &str) -> Result<Option<Self>, String> {
        let path = Self::path(dir, scenario);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map(Some)
                .map_err(|error| format!("{}: {error}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("{}: {error}", path.display())),
        }
    }

    /// Writes it (pretty, with a final newline): to a temporary file
    /// beside it first, then renamed over the old one, so a failed write
    /// never leaves a truncated snapshot.
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        let path = Self::path(dir, &self.scenario);
        let mut text = serde_json::to_string_pretty(self).map_err(|error| error.to_string())?;
        text.push('\n');
        std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        let temporary = dir.join(format!(".{}.json.tmp", self.scenario));
        let written =
            std::fs::write(&temporary, text).and_then(|()| std::fs::rename(&temporary, &path));
        written.map_err(|error| {
            let _ = std::fs::remove_file(&temporary);
            format!("{}: {error}", path.display())
        })
    }
}

/// How a run's steps differ from a snapshot's.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Diff {
    /// Differences that fail the comparison.
    pub mismatches: Vec<String>,
    /// Fields and headers the snapshot has and the run's response lacks:
    /// on the emulator, what it does not model. Listed, not failures.
    pub missing: Vec<String>,
    /// Fields and headers the run's response has and the snapshot lacks.
    /// Listed, not failures.
    pub extra: Vec<String>,
}

impl Diff {
    /// Compares a run's `actual` steps with the snapshot's `expected` ones.
    /// Field order never matters.
    pub fn of(expected: &[Step], actual: &[Step]) -> Self {
        let mut diff = Self::default();
        for (index, (want, got)) in expected.iter().zip(actual).enumerate() {
            let at = format!("#{index} {}", want.step);
            if want.step != got.step {
                diff.mismatches
                    .push(format!("{at}: the run's step is {:?}", got.step));
                continue;
            }
            if want.request != got.request {
                diff.mismatches.push(format!(
                    "{at}: request {:?}, the run sent {:?}",
                    want.request, got.request
                ));
            }
            if want.status != got.status {
                diff.mismatches.push(format!(
                    "{at}: status {:?}, the run got {:?}",
                    want.status, got.status
                ));
            }
            for (name, value) in &want.headers {
                match got.headers.get(name) {
                    None => diff.missing.push(format!("{at}: header {name}")),
                    Some(other) if other != value => diff.mismatches.push(format!(
                        "{at}: header {name}: {value:?}, the run got {other:?}"
                    )),
                    Some(_) => {}
                }
            }
            for name in got.headers.keys() {
                if !want.headers.contains_key(name) {
                    diff.extra.push(format!("{at}: header {name}"));
                }
            }
            compare(&format!("{at}: body"), &want.body, &got.body, &mut diff);
        }
        for extra in actual.iter().skip(expected.len()) {
            diff.mismatches.push(format!(
                "the run has a step the snapshot lacks: {:?}",
                extra.step
            ));
        }
        for missing in expected.iter().skip(actual.len()) {
            diff.mismatches.push(format!(
                "the run lacks the snapshot's step {:?}",
                missing.step
            ));
        }
        diff
    }

    /// Whether the comparison passes.
    pub fn passes(&self) -> bool {
        self.mismatches.is_empty()
    }
}

fn compare(at: &str, want: &Value, got: &Value, diff: &mut Diff) {
    match (want, got) {
        (Value::Object(want), Value::Object(got)) => {
            for (key, value) in want {
                match got.get(key) {
                    Some(other) => compare(&format!("{at}.{key}"), value, other, diff),
                    None => diff.missing.push(format!("{at}.{key}")),
                }
            }
            for key in got.keys() {
                if !want.contains_key(key) {
                    diff.extra.push(format!("{at}.{key}"));
                }
            }
        }
        (Value::Array(want), Value::Array(got)) => {
            if want.len() != got.len() {
                diff.mismatches.push(format!(
                    "{at}: {} items, the run has {}",
                    want.len(),
                    got.len()
                ));
            }
            for (index, (want, got)) in want.iter().zip(got).enumerate() {
                compare(&format!("{at}[{index}]"), want, got, diff);
            }
        }
        _ if want == got => {}
        _ => diff
            .mismatches
            .push(format!("{at}: {want}, the run has {got}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn step(name: &str, status: u16, body: Value) -> Step {
        Step {
            step: name.into(),
            request: Some("GET /app".into()),
            status: Some(status),
            headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
            body,
        }
    }

    #[test]
    fn snapshots_say_where_they_came_from_and_save_whole() {
        let dir = std::env::temp_dir().join(format!("octoemu-snapshots-{}", std::process::id()));
        for (provenance, source, live) in [
            (Provenance::Emulator, EMULATOR_SOURCE, false),
            (Provenance::GitHub, LIVE_SOURCE, true),
            (Provenance::Custom, CUSTOM_SOURCE, false),
        ] {
            let snapshot = Snapshot::new("s", provenance, vec![step("a", 200, json!({}))], None);
            assert_eq!(snapshot.source, source);
            assert_eq!(snapshot.is_live(), live);
            assert_eq!(
                snapshot.recorded.is_some(),
                provenance != Provenance::Emulator
            );
            snapshot.save(&dir).unwrap();
            assert_eq!(Snapshot::load(&dir, "s").unwrap(), Some(snapshot));
        }
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert_eq!(left.len(), 1, "no temporary file left");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn order_is_ignored_and_unmodelled_fields_are_listed_not_failed() {
        let want = [step(
            "a",
            200,
            json!({"a": 1, "b": {"c": 2, "live_only": true}}),
        )];
        let got = [step(
            "a",
            200,
            json!({"b": {"emulator_only": 1, "c": 2}, "a": 1}),
        )];
        let diff = Diff::of(&want, &got);
        assert!(diff.passes(), "{diff:?}");
        assert_eq!(diff.missing, ["#0 a: body.b.live_only"]);
        assert_eq!(diff.extra, ["#0 a: body.b.emulator_only"]);
    }

    #[test]
    fn statuses_values_lengths_and_steps_must_match() {
        let want = [
            step("a", 200, json!({"a": [1, 2]})),
            step("b", 204, Value::Null),
        ];
        let got = [step("a", 201, json!({"a": [1]}))];
        let diff = Diff::of(&want, &got);
        assert_eq!(diff.mismatches.len(), 3, "{diff:?}");
        let got = [
            step("a", 200, json!({"a": [1, 3]})),
            step("b", 204, Value::Null),
        ];
        assert_eq!(Diff::of(&want, &got).mismatches.len(), 1);
    }
}
