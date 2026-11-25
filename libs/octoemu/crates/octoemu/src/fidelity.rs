//! The fidelity manifest: how far each implemented route and each modelled
//! behaviour is known to match GitHub.
//!
//! `fidelity.toml` (next to the crate's `Cargo.toml`, compiled in) has one
//! entry per route (`[routes."GET /app/installations/{installation_id}"]`)
//! and per behaviour (`[behaviours.date-header]`), each with a label:
//!
//! | `label` | Required fields | Meaning |
//! |---|---|---|
//! | `verified` | `date`, `scenario` | The live fidelity suite measured it |
//! | `documented` | `doc` | GitHub's documentation (or the RFC it cites) says so |
//! | `assumed` | `reason` | A guess, refused in refuse-assumed mode |
//!
//! Every route the emulator registers must have an entry and every route
//! entry must name a registered route; the crate's tests and
//! [`crate::Emulator::start`] both check.
//!
//! `[scenarios]` maps each fidelity-suite scenario ID (the
//! `octoemu-conformance` crate) to the entries it measures: route keys
//! (`"GET /app"`) and behaviour IDs (`"jwt-error-messages"`). A live run
//! that passes a scenario stamps those entries `verified` with its ID; a
//! `verified` entry must name a listed scenario.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::sync::OnceLock;

/// The checked-in manifest's text.
const EMBEDDED: &str = include_str!("../fidelity.toml");

/// How far a route or behaviour is known to match GitHub.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "label", rename_all = "lowercase", deny_unknown_fields)]
pub enum Fidelity {
    /// Measured against GitHub by the live suite.
    Verified {
        /// When the scenario last passed against GitHub, as a quoted
        /// `"YYYY-MM-DD"` (a bare TOML date does not parse).
        date: NaiveDate,
        /// The scenario's ID.
        scenario: String,
    },
    /// Documented by GitHub.
    Documented {
        /// The documentation URL.
        doc: String,
    },
    /// Neither measured nor documented.
    Assumed {
        /// Why this guess, and what would settle it.
        reason: String,
    },
}

impl Fidelity {
    /// `verified`, `documented` or `assumed`.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Verified { .. } => "verified",
            Self::Documented { .. } => "documented",
            Self::Assumed { .. } => "assumed",
        }
    }

    /// Whether this is a guess.
    pub fn is_assumed(&self) -> bool {
        matches!(self, Self::Assumed { .. })
    }

    fn detail(&self) -> String {
        match self {
            Self::Verified { date, scenario } => format!("{date} {scenario}"),
            Self::Documented { doc } => doc.clone(),
            Self::Assumed { reason } => reason.clone(),
        }
    }
}

/// The parsed manifest.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Entries by route, `METHOD /pattern` (axum's `{param}` syntax, which
    /// is also GitHub's documentation's).
    #[serde(default)]
    pub routes: BTreeMap<String, Fidelity>,
    /// Entries by behaviour ID.
    #[serde(default)]
    pub behaviours: BTreeMap<String, Fidelity>,
    /// Fidelity-suite scenario IDs, each with the entries it measures
    /// (route keys and behaviour IDs).
    #[serde(default)]
    pub scenarios: BTreeMap<String, Vec<String>>,
}

/// A manifest that does not parse or is malformed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid fidelity manifest: {0}")]
pub struct ManifestError(String);

/// A registry and a manifest that disagree.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("fidelity manifest and code disagree: {}", .0.join("; "))]
pub struct Mismatch(pub Vec<String>);

const METHODS: &[&str] = &["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

impl Manifest {
    /// The checked-in `fidelity.toml`.
    pub fn embedded() -> Result<&'static Manifest, ManifestError> {
        static PARSED: OnceLock<Result<Manifest, ManifestError>> = OnceLock::new();
        PARSED
            .get_or_init(|| Self::parse(EMBEDDED))
            .as_ref()
            .map_err(Clone::clone)
    }

    /// Parses and checks a manifest.
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let manifest: Manifest =
            toml::from_str(text).map_err(|error| ManifestError(error.message().to_owned()))?;
        let mut problems = Vec::new();
        for key in manifest.routes.keys() {
            let well_formed = key.split_once(' ').is_some_and(|(method, path)| {
                METHODS.contains(&method) && path.starts_with('/') && !path.contains(' ')
            });
            if !well_formed {
                problems.push(format!("route {key:?} is not `METHOD /path`"));
            }
        }
        for key in manifest.behaviours.keys() {
            if !is_kebab(key) {
                problems.push(format!("behaviour {key:?} is not kebab-case"));
            }
        }
        for (scenario, covers) in &manifest.scenarios {
            if !is_kebab(scenario) {
                problems.push(format!("scenario {scenario:?} is not kebab-case"));
            }
            if covers.is_empty() {
                problems.push(format!("scenario {scenario:?} covers no entry"));
            }
            for key in covers {
                let known = if key.contains(' ') {
                    manifest.routes.contains_key(key)
                } else {
                    manifest.behaviours.contains_key(key)
                };
                if !known {
                    problems.push(format!(
                        "scenario {scenario:?} covers {key:?}, which has no entry"
                    ));
                }
            }
        }
        for (key, entry) in manifest.entries() {
            match entry {
                Fidelity::Verified { scenario, .. } if scenario.trim().is_empty() => {
                    problems.push(format!("{key}: `scenario` is empty"));
                }
                Fidelity::Verified { scenario, .. }
                    if !manifest.scenarios.contains_key(scenario) =>
                {
                    problems.push(format!(
                        "{key}: scenario {scenario:?} is not listed in [scenarios]"
                    ));
                }
                Fidelity::Documented { doc } if !doc.starts_with("https://") => {
                    problems.push(format!("{key}: `doc` must be an https URL"));
                }
                Fidelity::Assumed { reason } if reason.trim().is_empty() => {
                    problems.push(format!("{key}: `reason` is empty"));
                }
                _ => {}
            }
        }
        if problems.is_empty() {
            Ok(manifest)
        } else {
            Err(ManifestError(problems.join("; ")))
        }
    }

    /// Every entry with its display key (`GET /x` or `behaviour:id`).
    fn entries(&self) -> impl Iterator<Item = (String, &Fidelity)> {
        self.routes
            .iter()
            .map(|(key, entry)| (key.clone(), entry))
            .chain(
                self.behaviours
                    .iter()
                    .map(|(key, entry)| (format!("behaviour:{key}"), entry)),
            )
    }

    /// The entry for a route, `METHOD /pattern`.
    pub fn route(&self, route: &str) -> Option<&Fidelity> {
        self.routes.get(route)
    }

    /// The entry for a behaviour.
    pub fn behaviour(&self, id: &str) -> Option<&Fidelity> {
        self.behaviours.get(id)
    }

    /// The scenarios measuring an entry: a route key (`"GET /app"`) or a
    /// behaviour ID.
    pub fn scenarios_for(&self, key: &str) -> Vec<&str> {
        self.scenarios
            .iter()
            .filter(|(_, covers)| covers.iter().any(|covered| covered == key))
            .map(|(scenario, _)| scenario.as_str())
            .collect()
    }

    /// Checks that `routes` and `behaviours` (what the code implements) match
    /// the manifest's entries exactly.
    pub fn check<'a>(
        &self,
        routes: impl IntoIterator<Item = &'a str>,
        behaviours: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), Mismatch> {
        let mut problems = Vec::new();
        let mut compare = |kind: &str, code: Vec<&str>, manifest: &BTreeMap<String, Fidelity>| {
            for key in &code {
                if !manifest.contains_key(*key) {
                    problems.push(format!(
                        "{kind} {key:?} is implemented but has no fidelity.toml entry"
                    ));
                }
            }
            for key in manifest.keys() {
                if !code.contains(&key.as_str()) {
                    problems.push(format!(
                        "fidelity.toml lists {kind} {key:?}, which is not implemented"
                    ));
                }
            }
        };
        compare("route", routes.into_iter().collect(), &self.routes);
        compare(
            "behaviour",
            behaviours.into_iter().collect(),
            &self.behaviours,
        );
        if problems.is_empty() {
            Ok(())
        } else {
            Err(Mismatch(problems))
        }
    }

    /// The table `octoemu --fidelity` prints: every entry with its label,
    /// the fidelity-suite scenarios measuring it (`-` for none) and its
    /// detail (the verification date and scenario, the doc link or the
    /// reason).
    pub fn table(&self) -> String {
        let scenarios = |key: &str| {
            let ids = self.scenarios_for(key);
            if ids.is_empty() {
                "-".to_owned()
            } else {
                ids.join(",")
            }
        };
        let rows: Vec<(&str, String, &'static str, String, String)> = self
            .routes
            .iter()
            .map(|(key, entry)| {
                let covered = scenarios(key);
                ("route", key.clone(), entry.label(), covered, entry.detail())
            })
            .chain(self.behaviours.iter().map(|(key, entry)| {
                let covered = scenarios(key);
                (
                    "behaviour",
                    key.clone(),
                    entry.label(),
                    covered,
                    entry.detail(),
                )
            }))
            .collect();
        let width = rows
            .iter()
            .map(|row| row.1.len())
            .chain(["ID".len()])
            .max()
            .unwrap_or(2);
        let covered_width = rows
            .iter()
            .map(|row| row.3.len())
            .chain(["SCENARIOS".len()])
            .max()
            .unwrap_or(9);
        let mut out = String::new();
        let _ = writeln!(
            out,
            "{:<9}  {:<width$}  {:<10}  {:<covered_width$}  DETAIL",
            "KIND", "ID", "LABEL", "SCENARIOS"
        );
        for (kind, id, label, covered, detail) in &rows {
            let _ = writeln!(
                out,
                "{kind:<9}  {id:<width$}  {label:<10}  {covered:<covered_width$}  {detail}"
            );
        }
        let _ = writeln!(out, "\n{}", Summary::of(self));
        out
    }
}

fn is_kebab(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Counts per label.
struct Summary {
    routes: [usize; 3],
    behaviours: [usize; 3],
    scenarios: usize,
    covered: usize,
}

impl Summary {
    fn of(manifest: &Manifest) -> Self {
        let count = |entries: &BTreeMap<String, Fidelity>| {
            let mut counts = [0; 3];
            for entry in entries.values() {
                counts[match entry {
                    Fidelity::Verified { .. } => 0,
                    Fidelity::Documented { .. } => 1,
                    Fidelity::Assumed { .. } => 2,
                }] += 1;
            }
            counts
        };
        let covered: std::collections::BTreeSet<&String> =
            manifest.scenarios.values().flatten().collect();
        Self {
            routes: count(&manifest.routes),
            behaviours: count(&manifest.behaviours),
            scenarios: manifest.scenarios.len(),
            covered: covered.len(),
        }
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let part = |name: &str, [v, d, a]: [usize; 3]| {
            format!(
                "{name}: {} ({v} verified, {d} documented, {a} assumed)",
                v + d + a
            )
        };
        write!(
            f,
            "{}; {}; {} fidelity-suite scenarios measuring {} entries. \
             Unimplemented routes answer 501.",
            part("routes", self.routes),
            part("behaviours", self.behaviours),
            self.scenarios,
            self.covered
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_manifest_parses() {
        let manifest = Manifest::embedded().unwrap();
        assert!(!manifest.behaviours.is_empty());
        let table = manifest.table();
        assert!(table.starts_with("KIND"));
        assert!(table.contains("date-header"));
    }

    #[test]
    fn labels_need_their_fields() {
        let ok = Manifest::parse(
            r#"
            [routes."GET /app"]
            label = "verified"
            date = "2026-11-15"
            scenario = "sp-01"

            [behaviours.x-header]
            label = "assumed"
            reason = "nobody measured it"

            [behaviours.y]
            label = "documented"
            doc = "https://docs.github.com/rest"

            [scenarios]
            sp-01 = ["GET /app", "x-header"]
            "#,
        )
        .unwrap();
        assert_eq!(ok.route("GET /app").unwrap().label(), "verified");
        assert!(ok.behaviour("x-header").unwrap().is_assumed());
        assert_eq!(ok.scenarios_for("x-header"), ["sp-01"]);
        assert!(ok.scenarios_for("y").is_empty());
        assert!(ok.table().contains("sp-01"));

        for bad in [
            // A documented entry without its URL.
            "[behaviours.a]\nlabel = \"documented\"\n",
            // A field from another label.
            "[behaviours.a]\nlabel = \"assumed\"\nreason = \"r\"\ndoc = \"https://x\"\n",
            // An unknown label.
            "[behaviours.a]\nlabel = \"guessed\"\nreason = \"r\"\n",
            // Not an https URL.
            "[behaviours.a]\nlabel = \"documented\"\ndoc = \"docs.github.com\"\n",
            // An empty reason.
            "[behaviours.a]\nlabel = \"assumed\"\nreason = \" \"\n",
            // A malformed route key.
            "[routes.\"/app\"]\nlabel = \"assumed\"\nreason = \"r\"\n",
            "[routes.\"FETCH /app\"]\nlabel = \"assumed\"\nreason = \"r\"\n",
            // An unknown section.
            "[others.a]\nlabel = \"assumed\"\nreason = \"r\"\n",
            // A verified entry naming no listed scenario.
            "[behaviours.a]\nlabel = \"verified\"\ndate = \"2026-11-15\"\nscenario = \"s\"\n",
            // A scenario covering an unknown entry, or nothing.
            "[behaviours.a]\nlabel = \"assumed\"\nreason = \"r\"\n[scenarios]\ns = [\"b\"]\n",
            "[behaviours.a]\nlabel = \"assumed\"\nreason = \"r\"\n[scenarios]\ns = [\"GET /b\"]\n",
            "[behaviours.a]\nlabel = \"assumed\"\nreason = \"r\"\n[scenarios]\ns = []\n",
            "[behaviours.a]\nlabel = \"assumed\"\nreason = \"r\"\n[scenarios]\nS_1 = [\"a\"]\n",
        ] {
            assert!(Manifest::parse(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn check_reports_both_directions() {
        let manifest = Manifest::parse(
            "[routes.\"GET /app\"]\nlabel = \"assumed\"\nreason = \"r\"\n\
             [behaviours.b]\nlabel = \"assumed\"\nreason = \"r\"\n",
        )
        .unwrap();
        assert!(manifest.check(["GET /app"], ["b"]).is_ok());
        let Mismatch(problems) = manifest.check(["POST /app"], []).unwrap_err();
        assert_eq!(problems.len(), 3, "{problems:?}");
    }
}
