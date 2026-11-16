//! Fault rules: injected failures on the GitHub surfaces (API, web pages,
//! avatars; never the admin API).
//!
//! A rule matches requests by method, path pattern, credential kind and,
//! optionally, a substring of the request body, counts its matches, and
//! fires on the `nth` match for `times` matches (or every one after). A
//! firing rule's [`FaultAction`] answers instead of, or around, the real
//! handler: a canned response, a delay, a hold until released, the real
//! handler with its response lost, or a closed or reset connection.
//!
//! **Fault-injected responses are not GitHub behaviour.** They carry no
//! fidelity label; they exist so a client's failure handling can be tested.
//! Every call a rule fires on is marked in the call log
//! ([`crate::calls::Call::fault`]).
//!
//! Rules are kept in the order they were added; the first rule that fires
//! decides. A rule that matches but does not fire (before its `nth` match,
//! or after its `times`) still counts the match and lets later rules look.

use crate::calls::CredentialKind;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// One fault rule, as `POST /__octoemu/faults` takes it.
///
/// ```json
/// {"method": "PUT", "path": "/repos/{owner}/{repo}/collaborators/{username}",
///  "credential": "installation_token", "body_contains": "\"admin\"",
///  "nth": 1, "times": 1,
///  "action": {"kind": "respond", "status": 502}}
/// ```
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[must_use]
pub struct Fault {
    /// The HTTP method (any case); every method when absent or `"*"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// The path pattern, matched against the request path (the query is
    /// ignored): literal segments, axum-style `{param}` (one segment, named
    /// for [`FaultRequest::params`]) or `{*rest}` (the rest of the path), and
    /// globs: `*` within or as a segment (one segment), `**` (any number of
    /// segments).
    pub path: String,
    /// Only requests whose `Authorization` header has this shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<CredentialKind>,
    /// Only requests whose body contains this text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_contains: Option<String>,
    /// The first matching request the rule fires on, from 1 (the default).
    #[serde(default = "first", skip_serializing_if = "is_first")]
    pub nth: u64,
    /// How many matching requests it fires on; every one from the `nth`
    /// when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub times: Option<u64>,
    /// A free-text label, listed back and usable to find the rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// What happens when it fires.
    pub action: FaultAction,
}

fn first() -> u64 {
    1
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if signature
fn is_first(nth: &u64) -> bool {
    *nth == 1
}

impl Fault {
    /// A rule for every request to `path` (see [`Fault::path`]) that answers
    /// with `action`.
    pub fn new(path: impl Into<String>, action: FaultAction) -> Self {
        Self {
            method: None,
            path: path.into(),
            credential: None,
            body_contains: None,
            nth: 1,
            times: None,
            label: None,
            action,
        }
    }

    /// A rule for `method` requests to `path`.
    pub fn on(method: &str, path: impl Into<String>, action: FaultAction) -> Self {
        Self::new(path, action).method(method)
    }

    /// Only `method` requests.
    pub fn method(mut self, method: &str) -> Self {
        self.method = Some(method.to_ascii_uppercase());
        self
    }

    /// Only requests with this kind of credential.
    pub fn credential(mut self, kind: CredentialKind) -> Self {
        self.credential = Some(kind);
        self
    }

    /// Only requests whose body contains `text`.
    pub fn body_contains(mut self, text: impl Into<String>) -> Self {
        self.body_contains = Some(text.into());
        self
    }

    /// Fires from the `n`th matching request (1 is the first).
    pub fn nth(mut self, n: u64) -> Self {
        self.nth = n.max(1);
        self
    }

    /// Fires on `n` matching requests, then lets them through.
    pub fn times(mut self, n: u64) -> Self {
        self.times = Some(n);
        self
    }

    /// Fires once.
    pub fn once(self) -> Self {
        self.times(1)
    }

    /// Labels the rule.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

/// What a firing rule does.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FaultAction {
    /// Answers with this response; the real handler does not run, so
    /// nothing changes.
    Respond(Reply),
    /// Waits, then runs the real handler: `ms` milliseconds of real time,
    /// or, with `clock: "virtual"`, moves the virtual clock forward by
    /// that much first (so the response's `Date` and every expiry see the
    /// time pass).
    Delay {
        /// How long.
        ms: u64,
        /// Which clock.
        #[serde(default)]
        clock: DelayClock,
    },
    /// Holds the request until the rule is released
    /// (`POST /__octoemu/faults/{id}/release`, [`crate::Emulator::release_fault`]),
    /// removed, or the emulator stops; then runs the real handler. A
    /// released rule holds no more requests.
    Hold,
    /// Runs the real handler (its changes happen), then loses its response:
    /// answers `status` instead (with a short text body), or, without a
    /// `status`, closes the connection without answering.
    ApplyThenLose {
        /// The status sent instead, e.g. 502; none closes the connection.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
    },
    /// Closes the connection without a response; the handler does not run.
    Close,
    /// Resets the connection (TCP RST); the handler does not run.
    Reset,
    /// Answers with what a Rust closure returns for the request (embedded
    /// mode only; listed as `{"kind": "custom"}`). The handler does not run.
    #[serde(skip)]
    Custom(Responder),
}

impl FaultAction {
    /// [`FaultAction::Respond`] with `status` and no body.
    pub fn respond(status: u16) -> Self {
        Self::Respond(Reply::new(status))
    }

    /// [`FaultAction::Respond`] with `status` and a JSON body.
    pub fn respond_json(status: u16, body: Value) -> Self {
        Self::Respond(Reply::new(status).json(body))
    }

    /// [`FaultAction::Custom`] from a closure.
    pub fn custom(responder: impl Fn(&FaultRequest) -> Reply + Send + Sync + 'static) -> Self {
        Self::Custom(Responder(Arc::new(responder)))
    }

    /// The action's `kind`, as the call log and the listing name it.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Respond(_) => "respond",
            Self::Delay { .. } => "delay",
            Self::Hold => "hold",
            Self::ApplyThenLose { .. } => "apply_then_lose",
            Self::Close => "close",
            Self::Reset => "reset",
            Self::Custom(_) => "custom",
        }
    }

    fn view(&self) -> Value {
        match self {
            Self::Custom(_) => json!({"kind": "custom"}),
            action => serde_json::to_value(action).unwrap_or(Value::Null),
        }
    }
}

/// Which clock a [`FaultAction::Delay`] uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelayClock {
    /// Real time: the response arrives that much later.
    #[default]
    Real,
    /// The virtual clock jumps forward; the response is not slowed down.
    Virtual,
}

/// A canned response.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[must_use]
pub struct Reply {
    /// The status code.
    pub status: u16,
    /// Extra headers.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// A JSON body (sent as `application/json; charset=utf-8`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
    /// A text body, sent as is (instead of `body`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl Reply {
    /// `status` with no body.
    pub fn new(status: u16) -> Self {
        Self {
            status,
            ..Self::default()
        }
    }

    /// Adds a header.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    /// Sets a JSON body.
    pub fn json(mut self, body: Value) -> Self {
        self.body = Some(body);
        self
    }

    /// Sets a text body.
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }
}

/// A closure answering for [`FaultAction::Custom`].
#[derive(Clone)]
pub struct Responder(Arc<dyn Fn(&FaultRequest) -> Reply + Send + Sync>);

impl fmt::Debug for Responder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Responder(..)")
    }
}

impl Responder {
    pub(crate) fn reply(&self, request: &FaultRequest) -> Reply {
        (self.0)(request)
    }
}

/// The request a rule fired on, as a [`FaultAction::Custom`] closure sees it.
#[derive(Clone, Debug)]
pub struct FaultRequest {
    /// The method.
    pub method: String,
    /// The path (no query).
    pub path: String,
    /// The pattern's named segments (`{param}`, `{*rest}`).
    pub params: BTreeMap<String, String>,
    /// The credential's shape.
    pub credential: CredentialKind,
}

/// A rule as listed by `GET /__octoemu/faults` and [`crate::Emulator::faults`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FaultStatus {
    /// The rule's ID.
    pub id: u64,
    /// The method it matches (`null`: any).
    pub method: Option<String>,
    /// The path pattern.
    pub path: String,
    /// The credential kind it matches (`null`: any).
    pub credential: Option<CredentialKind>,
    /// The body text it requires.
    pub body_contains: Option<String>,
    /// The first match it fires on.
    pub nth: u64,
    /// How many times it fires (`null`: unlimited).
    pub times: Option<u64>,
    /// Its label.
    pub label: Option<String>,
    /// The action (`{"kind": "custom"}` for a closure).
    pub action: Value,
    /// Requests that matched so far.
    pub matched: u64,
    /// Requests it fired on so far.
    pub fired: u64,
    /// Requests it holds now ([`FaultAction::Hold`]).
    pub held: usize,
    /// Whether it was released ([`FaultAction::Hold`]).
    pub released: bool,
}

/// Why a rule was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("octoemu: invalid fault rule: {0}")]
pub struct FaultError(String);

/// One segment of a compiled path pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Segment {
    Literal(String),
    Param(String),
    Rest(String),
    Glob(String),
    Any,
}

/// A compiled path pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pattern(Vec<Segment>);

impl Pattern {
    pub(crate) fn parse(pattern: &str) -> Result<Self, FaultError> {
        let Some(rest) = pattern.strip_prefix('/') else {
            return Err(FaultError(format!("path {pattern:?} must start with '/'")));
        };
        let raw: Vec<&str> = if rest.is_empty() {
            Vec::new()
        } else {
            rest.split('/').collect()
        };
        let mut segments = Vec::with_capacity(raw.len());
        for (index, segment) in raw.iter().enumerate() {
            let last = index + 1 == raw.len();
            let parsed = if let Some(name) =
                segment.strip_prefix("{*").and_then(|s| s.strip_suffix('}'))
            {
                if !last {
                    return Err(FaultError(format!(
                        "{{*{name}}} must be the last segment of {pattern:?}"
                    )));
                }
                Segment::Rest(name.to_owned())
            } else if let Some(name) = segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                if name.is_empty() || name.contains(['{', '}']) {
                    return Err(FaultError(format!("bad parameter in {pattern:?}")));
                }
                Segment::Param(name.to_owned())
            } else if *segment == "**" {
                Segment::Any
            } else if segment.contains(['{', '}']) {
                return Err(FaultError(format!("bad parameter in {pattern:?}")));
            } else if segment.contains('*') {
                Segment::Glob((*segment).to_owned())
            } else {
                Segment::Literal((*segment).to_owned())
            };
            segments.push(parsed);
        }
        Ok(Self(segments))
    }

    /// The named segments when `path` matches.
    pub(crate) fn matches(&self, path: &str) -> Option<BTreeMap<String, String>> {
        let rest = path.strip_prefix('/')?;
        let parts: Vec<&str> = if rest.is_empty() {
            Vec::new()
        } else {
            rest.split('/').collect()
        };
        let mut params = BTreeMap::new();
        match_segments(&self.0, &parts, &mut params).then_some(params)
    }
}

fn match_segments(
    pattern: &[Segment],
    parts: &[&str],
    params: &mut BTreeMap<String, String>,
) -> bool {
    let Some((head, tail)) = pattern.split_first() else {
        return parts.is_empty();
    };
    match head {
        Segment::Any => (0..=parts.len()).any(|skip| {
            let mut attempt = params.clone();
            let found = match_segments(tail, &parts[skip..], &mut attempt);
            if found {
                *params = attempt;
            }
            found
        }),
        Segment::Rest(name) => {
            if parts.is_empty() {
                return false;
            }
            params.insert(name.clone(), parts.join("/"));
            true
        }
        _ => {
            let Some((part, others)) = parts.split_first() else {
                return false;
            };
            let ok = match head {
                Segment::Literal(literal) => literal == part,
                Segment::Param(name) => {
                    if part.is_empty() {
                        false
                    } else {
                        params.insert(name.clone(), (*part).to_owned());
                        true
                    }
                }
                Segment::Glob(glob) => glob_matches(glob.as_bytes(), part.as_bytes()),
                Segment::Any | Segment::Rest(_) => unreachable!("handled above"),
            };
            ok && match_segments(tail, others, params)
        }
    }
}

/// `*` (any run of characters) within one segment.
fn glob_matches(glob: &[u8], text: &[u8]) -> bool {
    match glob.split_first() {
        None => text.is_empty(),
        Some((b'*', rest)) => (0..=text.len()).any(|skip| glob_matches(rest, &text[skip..])),
        Some((c, rest)) => text
            .split_first()
            .is_some_and(|(t, others)| t == c && glob_matches(rest, others)),
    }
}

/// A hold gate: whether the rule was released, and how many requests wait.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct GateState {
    pub(crate) released: bool,
    pub(crate) held: usize,
}

#[derive(Debug)]
pub(crate) struct Gate(watch::Sender<GateState>);

impl Gate {
    fn new() -> Self {
        Self(watch::Sender::new(GateState::default()))
    }

    pub(crate) fn release(&self) {
        self.0.send_modify(|state| state.released = true);
    }

    fn state(&self) -> GateState {
        *self.0.borrow()
    }

    /// Waits until released (counting itself as held meanwhile).
    pub(crate) async fn pass(&self) {
        let mut released = self.0.subscribe();
        if released.borrow().released {
            return;
        }
        self.0.send_modify(|state| state.held += 1);
        let _ = released.wait_for(|state| state.released).await;
        self.0
            .send_modify(|state| state.held = state.held.saturating_sub(1));
    }

    /// Waits until at least `n` requests are held, at most `timeout`.
    pub(crate) async fn held(&self, n: usize, timeout: Duration) -> bool {
        let mut state = self.0.subscribe();
        tokio::time::timeout(timeout, state.wait_for(|state| state.held >= n))
            .await
            .is_ok_and(|result| result.is_ok())
    }
}

#[derive(Debug)]
struct Rule {
    id: u64,
    fault: Fault,
    pattern: Pattern,
    method: Option<String>,
    matched: u64,
    fired: u64,
    gate: Arc<Gate>,
}

impl Rule {
    fn status(&self) -> FaultStatus {
        let gate = self.gate.state();
        FaultStatus {
            id: self.id,
            method: self.method.clone(),
            path: self.fault.path.clone(),
            credential: self.fault.credential,
            body_contains: self.fault.body_contains.clone(),
            nth: self.fault.nth,
            times: self.fault.times,
            label: self.fault.label.clone(),
            action: self.fault.action.view(),
            matched: self.matched,
            fired: self.fired,
            held: gate.held,
            released: gate.released,
        }
    }

    fn selects(&self, method: &str, path: &str, credential: CredentialKind) -> bool {
        self.method.as_deref().is_none_or(|m| m == method)
            && self.fault.credential.is_none_or(|kind| kind == credential)
            && self.pattern.matches(path).is_some()
    }
}

/// What the request looks like, for matching.
pub(crate) struct Probe<'a> {
    pub(crate) method: &'a str,
    pub(crate) path: &'a str,
    pub(crate) credential: CredentialKind,
    pub(crate) body: &'a [u8],
}

/// A rule that fired.
pub(crate) struct Fired {
    pub(crate) id: u64,
    pub(crate) action: FaultAction,
    pub(crate) request: FaultRequest,
    pub(crate) gate: Arc<Gate>,
}

/// Every rule of an instance.
#[derive(Debug, Default)]
pub(crate) struct Faults {
    rules: Vec<Rule>,
    last_id: u64,
}

impl Faults {
    pub(crate) fn add(&mut self, fault: Fault) -> Result<u64, FaultError> {
        let pattern = Pattern::parse(&fault.path)?;
        let status = match &fault.action {
            FaultAction::Respond(reply) => Some(reply.status),
            FaultAction::ApplyThenLose { status } => *status,
            _ => None,
        };
        if let Some(status) = status
            && axum::http::StatusCode::from_u16(status).is_err()
        {
            return Err(FaultError(format!("status {status} is not an HTTP status")));
        }
        if let FaultAction::Respond(reply) = &fault.action {
            for (name, value) in &reply.headers {
                if axum::http::HeaderName::from_bytes(name.as_bytes()).is_err()
                    || axum::http::HeaderValue::from_str(value).is_err()
                {
                    return Err(FaultError(format!("bad header {name:?}")));
                }
            }
        }
        let method = match fault.method.as_deref() {
            None | Some("*") => None,
            Some(method) => {
                let method = method.to_ascii_uppercase();
                if axum::http::Method::from_bytes(method.as_bytes()).is_err() {
                    return Err(FaultError(format!("bad method {method:?}")));
                }
                Some(method)
            }
        };
        self.last_id += 1;
        let id = self.last_id;
        self.rules.push(Rule {
            id,
            fault,
            pattern,
            method,
            matched: 0,
            fired: 0,
            gate: Arc::new(Gate::new()),
        });
        Ok(id)
    }

    pub(crate) fn list(&self) -> Vec<FaultStatus> {
        self.rules.iter().map(Rule::status).collect()
    }

    pub(crate) fn get(&self, id: u64) -> Option<FaultStatus> {
        self.rules
            .iter()
            .find(|rule| rule.id == id)
            .map(Rule::status)
    }

    pub(crate) fn gate(&self, id: u64) -> Option<Arc<Gate>> {
        self.rules
            .iter()
            .find(|rule| rule.id == id)
            .map(|rule| rule.gate.clone())
    }

    /// Removes rule `id`, releasing what it holds.
    pub(crate) fn remove(&mut self, id: u64) -> bool {
        let before = self.rules.len();
        self.rules.retain(|rule| {
            if rule.id == id {
                rule.gate.release();
            }
            rule.id != id
        });
        self.rules.len() != before
    }

    /// Removes every rule labelled `label`.
    pub(crate) fn remove_labelled(&mut self, label: &str) {
        let ids: Vec<u64> = self
            .rules
            .iter()
            .filter(|rule| rule.fault.label.as_deref() == Some(label))
            .map(|rule| rule.id)
            .collect();
        for id in ids {
            self.remove(id);
        }
    }

    /// Removes every rule, releasing what they hold.
    pub(crate) fn clear(&mut self) {
        for rule in &self.rules {
            rule.gate.release();
        }
        self.rules.clear();
    }

    pub(crate) fn release(&self, id: u64) -> bool {
        match self.rules.iter().find(|rule| rule.id == id) {
            Some(rule) => {
                rule.gate.release();
                true
            }
            None => false,
        }
    }

    /// Counts the request against every selecting rule; the first that
    /// fires decides.
    pub(crate) fn decide(&mut self, probe: &Probe<'_>) -> Option<Fired> {
        for rule in &mut self.rules {
            if !rule.selects(probe.method, probe.path, probe.credential) {
                continue;
            }
            if let Some(text) = &rule.fault.body_contains
                && !contains(probe.body, text.as_bytes())
            {
                continue;
            }
            rule.matched += 1;
            let due = rule.matched >= rule.fault.nth;
            let left = rule.fault.times.is_none_or(|times| rule.fired < times);
            if !(due && left) {
                continue;
            }
            rule.fired += 1;
            let params = rule.pattern.matches(probe.path).unwrap_or_default();
            return Some(Fired {
                id: rule.id,
                action: rule.fault.action.clone(),
                request: FaultRequest {
                    method: probe.method.to_owned(),
                    path: probe.path.to_owned(),
                    params,
                    credential: probe.credential,
                },
                gate: rule.gate.clone(),
            });
        }
        None
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty()
        || haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pattern: &str, path: &str) -> Option<Vec<(String, String)>> {
        Pattern::parse(pattern)
            .unwrap()
            .matches(path)
            .map(|params| params.into_iter().collect())
    }

    #[test]
    fn matches_axum_patterns_and_globs() {
        let pair = |k: &str, v: &str| (k.to_owned(), v.to_owned());
        assert_eq!(
            params(
                "/repos/{owner}/{repo}/collaborators/{username}",
                "/repos/acme/api/collaborators/alice"
            ),
            Some(vec![
                pair("owner", "acme"),
                pair("repo", "api"),
                pair("username", "alice")
            ])
        );
        assert_eq!(params("/repos/{owner}/{repo}", "/repos/acme"), None);
        assert_eq!(params("/repos/{owner}/{repo}", "/repos/acme/api/x"), None);
        assert_eq!(params("/repos/*/api", "/repos/acme/api"), Some(vec![]));
        assert_eq!(params("/repos/ac*e/api", "/repos/acme/api"), Some(vec![]));
        assert_eq!(params("/repos/ac*e/api", "/repos/acne/web"), None);
        assert_eq!(
            params("/repos/**", "/repos/acme/api/invitations/1"),
            Some(vec![])
        );
        assert_eq!(
            params("/repos/**/invitations/{id}", "/repos/a/b/invitations/7"),
            Some(vec![pair("id", "7")])
        );
        assert_eq!(params("/**", "/"), Some(vec![]));
        assert_eq!(
            params("/app/{*rest}", "/app/hook/config"),
            Some(vec![pair("rest", "hook/config")])
        );
        assert_eq!(params("/app/{*rest}", "/app"), None);
        assert_eq!(params("/user", "/user"), Some(vec![]));
        assert_eq!(params("/user", "/users"), None);
        assert!(Pattern::parse("repos").is_err());
        assert!(Pattern::parse("/a/{*rest}/b").is_err());
        assert!(Pattern::parse("/a/{b").is_err());
    }

    fn probe<'a>(method: &'a str, path: &'a str) -> Probe<'a> {
        Probe {
            method,
            path,
            credential: CredentialKind::InstallationToken,
            body: b"{\"permission\":\"push\"}",
        }
    }

    #[test]
    fn counts_nth_and_times() {
        let mut faults = Faults::default();
        let id = faults
            .add(
                Fault::on("put", "/r/{x}", FaultAction::respond(502))
                    .nth(2)
                    .times(2),
            )
            .unwrap();
        let fired: Vec<bool> = (0..5)
            .map(|_| faults.decide(&probe("PUT", "/r/1")).is_some())
            .collect();
        assert_eq!(fired, [false, true, true, false, false]);
        assert!(faults.decide(&probe("GET", "/r/1")).is_none());
        let status = faults.get(id).unwrap();
        assert_eq!((status.matched, status.fired), (5, 2));
        assert_eq!(status.method.as_deref(), Some("PUT"));
    }

    #[test]
    fn falls_through_to_later_rules_and_filters_by_body_and_credential() {
        let mut faults = Faults::default();
        faults
            .add(Fault::new("/r/{x}", FaultAction::respond(500)).body_contains("admin"))
            .unwrap();
        faults
            .add(Fault::new("/r/{x}", FaultAction::respond(501)).credential(CredentialKind::Jwt))
            .unwrap();
        let third = faults
            .add(Fault::new("/r/{x}", FaultAction::respond(503)).once())
            .unwrap();
        let fired = faults.decide(&probe("PUT", "/r/1")).unwrap();
        assert_eq!(fired.id, third);
        assert_eq!(fired.request.params["x"], "1");
        assert!(faults.decide(&probe("PUT", "/r/1")).is_none());
    }

    #[test]
    fn refuses_bad_rules() {
        let mut faults = Faults::default();
        assert!(
            faults
                .add(Fault::new("/x", FaultAction::respond(42)))
                .is_err()
        );
        assert!(
            faults
                .add(Fault::new(
                    "/x",
                    FaultAction::Respond(Reply::new(200).header("bad name", "v"))
                ))
                .is_err()
        );
        assert!(
            faults
                .add(Fault::on("GE T", "/x", FaultAction::Close))
                .is_err()
        );
        let rule: Fault = serde_json::from_value(json!({
            "path": "/x", "action": {"kind": "apply_then_lose"}
        }))
        .unwrap();
        assert!(faults.add(rule).is_ok());
        assert!(
            serde_json::from_value::<Fault>(json!({"path": "/x", "action": {"kind": "custom"}}))
                .is_err()
        );
    }
}
