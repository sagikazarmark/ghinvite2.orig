//! What a scenario runs with: an HTTP client for the target's public
//! routes, App JWTs, recording, and the clock.

use crate::normalise::{Normaliser, RELIED_HEADERS};
use crate::schema::{Finding, ResponseBody, Schemas};
use crate::snapshot::Step;
use crate::target::{TargetKind, World};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use octoemu::Emulator;
use reqwest::Method;
use reqwest::header::HeaderMap;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::signature::{SignatureEncoding, Signer};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

/// The API version every request sends unless a step says otherwise
/// (ghinvite's pin).
pub const API_VERSION: &str = "2022-11-28";

/// Why a scenario stopped before its end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stop {
    /// It cannot run on this target (or not without an opt-in): the reason.
    Skip(String),
    /// Something it depends on did not happen: the reason.
    Fail(String),
}

/// Stops the scenario with a failure.
pub fn fail<T>(reason: impl Into<String>) -> Result<T, Stop> {
    Err(Stop::Fail(reason.into()))
}

/// Skips the scenario.
pub fn skip<T>(reason: impl Into<String>) -> Result<T, Stop> {
    Err(Stop::Skip(reason.into()))
}

/// Which host a request goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Base {
    /// The REST API.
    Api,
    /// github.com's web routes (`/login/oauth/…`).
    Web,
}

/// The request's credential.
#[derive(Clone, Debug)]
pub enum Cred {
    /// None.
    None,
    /// `Authorization: Bearer …`.
    Bearer(String),
    /// `Authorization: token …`.
    Token(String),
    /// `Authorization: Basic base64(id:secret)`.
    Basic(String, String),
}

/// A request body.
#[derive(Clone, Debug)]
pub enum Body {
    /// None.
    None,
    /// JSON.
    Json(Value),
    /// `application/x-www-form-urlencoded`.
    Form(Vec<(String, String)>),
}

/// A request to the target's public routes.
#[derive(Clone, Debug)]
pub struct Req {
    method: Method,
    base: Base,
    path: String,
    cred: Cred,
    headers: Vec<(String, String)>,
    body: Body,
}

impl Req {
    /// `method path` on the REST API (`path` may carry a query).
    pub fn new(method: Method, path: impl Into<String>) -> Self {
        let path = path.into();
        assert!(
            !path.starts_with("/__octoemu"),
            "scenario steps use public GitHub routes only, never the admin API"
        );
        Self {
            method,
            base: Base::Api,
            path,
            cred: Cred::None,
            headers: Vec::new(),
            body: Body::None,
        }
    }

    /// `GET path`.
    pub fn get(path: impl Into<String>) -> Self {
        Self::new(Method::GET, path)
    }

    /// `POST path`.
    pub fn post(path: impl Into<String>) -> Self {
        Self::new(Method::POST, path)
    }

    /// `PUT path`.
    pub fn put(path: impl Into<String>) -> Self {
        Self::new(Method::PUT, path)
    }

    /// `PATCH path`.
    pub fn patch(path: impl Into<String>) -> Self {
        Self::new(Method::PATCH, path)
    }

    /// `DELETE path`.
    pub fn delete(path: impl Into<String>) -> Self {
        Self::new(Method::DELETE, path)
    }

    /// Sends it to the web base instead.
    #[must_use]
    pub fn web(mut self) -> Self {
        self.base = Base::Web;
        self
    }

    /// `Authorization: Bearer token`.
    #[must_use]
    pub fn bearer(mut self, token: impl Into<String>) -> Self {
        self.cred = Cred::Bearer(token.into());
        self
    }

    /// `Authorization: token token`.
    #[must_use]
    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.cred = Cred::Token(token.into());
        self
    }

    /// Basic client credentials.
    #[must_use]
    pub fn basic(mut self, id: impl Into<String>, secret: impl Into<String>) -> Self {
        self.cred = Cred::Basic(id.into(), secret.into());
        self
    }

    /// An extra header (replacing a default of the same name).
    #[must_use]
    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_ascii_lowercase(), value.into()));
        self
    }

    /// A JSON body.
    #[must_use]
    pub fn json(mut self, body: Value) -> Self {
        self.body = Body::Json(body);
        self
    }

    /// A form body.
    #[must_use]
    pub fn form(mut self, pairs: &[(&str, &str)]) -> Self {
        self.body = Body::Form(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        );
        self
    }

    fn path_only(&self) -> &str {
        self.path.split(['?', '#']).next().unwrap_or("")
    }
}

/// A response.
#[derive(Clone, Debug)]
pub struct Reply {
    /// The status.
    pub status: u16,
    /// The headers.
    pub headers: HeaderMap,
    /// The body as JSON; `{"$form": …}` / `{"$text": …}` otherwise; `null`
    /// when empty.
    pub body: Value,
    /// Whether the body was JSON.
    pub is_json: bool,
}

impl Reply {
    /// A header's value.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// `body[field]` as a number.
    pub fn u64(&self, field: &str) -> Option<u64> {
        self.body.get(field).and_then(Value::as_u64)
    }

    /// `body[field]` as a string.
    pub fn str(&self, field: &str) -> Option<&str> {
        self.body.get(field).and_then(Value::as_str)
    }

    /// The `Date` header as a Unix time.
    pub fn date(&self) -> Option<i64> {
        let date = self.header("date")?;
        chrono::DateTime::parse_from_rfc2822(date)
            .ok()
            .map(|at| at.timestamp())
    }

    /// Reads a response whole.
    pub async fn read(response: reqwest::Response) -> Result<Self, reqwest::Error> {
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let bytes = response.bytes().await?;
        let content_type = headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let (body, is_json) = if bytes.is_empty() {
            (Value::Null, false)
        } else if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
            (value, true)
        } else if content_type.starts_with("application/x-www-form-urlencoded") {
            let form: serde_json::Map<String, Value> = url::form_urlencoded::parse(&bytes)
                .map(|(k, v)| (k.into_owned(), Value::String(v.into_owned())))
                .collect();
            (json!({"$form": form}), false)
        } else {
            (
                json!({"$text": String::from_utf8_lossy(&bytes).chars().take(2000).collect::<String>()}),
                false,
            )
        };
        Ok(Self {
            status,
            headers,
            body,
            is_json,
        })
    }
}

/// The scenario's context.
pub struct Ctx {
    http: reqwest::Client,
    world: World,
    kind: TargetKind,
    emulator: Option<Arc<Emulator>>,
    normaliser: Normaliser,
    schemas: Option<Arc<Schemas>>,
    slow: bool,
    pub(crate) steps: Vec<Step>,
    pub(crate) schema_findings: Vec<(String, Finding)>,
}

impl Ctx {
    pub(crate) fn new(
        world: World,
        kind: TargetKind,
        emulator: Option<Arc<Emulator>>,
        schemas: Option<Arc<Schemas>>,
        slow: bool,
    ) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("octoemu-conformance")
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            http,
            normaliser: Normaliser::new(),
            world,
            kind,
            emulator,
            schemas,
            slow,
            steps: Vec::new(),
            schema_findings: Vec::new(),
        })
    }

    /// The world.
    pub fn world(&self) -> &World {
        &self.world
    }

    /// Whether this runs against GitHub.
    pub fn is_live(&self) -> bool {
        self.kind == TargetKind::Live
    }

    /// Which target.
    pub fn kind(&self) -> TargetKind {
        self.kind
    }

    pub(crate) fn set_known(&mut self, known: crate::target::Known) {
        self.world.known = known;
        self.normaliser = normaliser_for(&self.world);
    }

    /// Sends `req` and records it as step `name` of the snapshot.
    pub async fn send(&mut self, name: &str, req: Req) -> Result<Reply, Stop> {
        let request_line = self.request_line(&req);
        let reply = self.call(req).await?;
        self.record_reply(name, request_line, &reply);
        Ok(reply)
    }

    /// `req`'s normalised request line, as [`Ctx::send`] records it.
    pub fn request_line(&self, req: &Req) -> String {
        format!("{} {}", req.method, self.normaliser.path(&req.path))
    }

    /// Records a response obtained another way (or with [`Ctx::call`]) as
    /// step `name`.
    pub fn record_reply(&mut self, name: &str, request_line: String, reply: &Reply) {
        let mut headers = std::collections::BTreeMap::new();
        for name in RELIED_HEADERS {
            if let Some(value) = reply.header(name) {
                headers.insert((*name).to_owned(), self.normaliser.header(name, value));
            }
        }
        self.record(Step {
            step: name.to_owned(),
            request: Some(request_line),
            status: Some(reply.status),
            headers,
            body: self.normaliser.value(&reply.body),
        });
    }

    /// Records a fact derived from responses (a count, a token's shape) as
    /// step `name`.
    pub fn observe(&mut self, name: &str, value: Value) {
        let body = self.normaliser.value(&value);
        self.record(Step {
            step: name.to_owned(),
            request: None,
            status: None,
            headers: Default::default(),
            body,
        });
    }

    fn record(&mut self, step: Step) {
        assert!(
            self.steps.iter().all(|s| s.step != step.step),
            "step {:?} recorded twice",
            step.step
        );
        self.steps.push(step);
    }

    /// Sends `req` without recording it (setup, cleanup, polling). The
    /// schema check still sees it.
    pub async fn call(&mut self, req: Req) -> Result<Reply, Stop> {
        let base = match req.base {
            Base::Api => &self.world.api_base,
            Base::Web => &self.world.web_base,
        };
        let url = format!("{}{}", base.trim_end_matches('/'), req.path);
        let mut builder = self.http.request(req.method.clone(), &url);
        let mut defaults: Vec<(String, String)> = Vec::new();
        if req.base == Base::Api {
            defaults.push(("accept".into(), "application/vnd.github+json".into()));
            defaults.push(("x-github-api-version".into(), API_VERSION.into()));
        } else {
            defaults.push(("accept".into(), "application/json".into()));
        }
        for (name, value) in defaults {
            if !req.headers.iter().any(|(n, _)| *n == name) {
                builder = builder.header(name, value);
            }
        }
        for (name, value) in &req.headers {
            builder = builder.header(name, value);
        }
        builder = match &req.cred {
            Cred::None => builder,
            Cred::Bearer(token) => builder.header("authorization", format!("Bearer {token}")),
            Cred::Token(token) => builder.header("authorization", format!("token {token}")),
            Cred::Basic(id, secret) => builder.header(
                "authorization",
                format!("Basic {}", STANDARD.encode(format!("{id}:{secret}"))),
            ),
        };
        builder = match &req.body {
            Body::None => builder,
            Body::Json(value) => builder
                .header("content-type", "application/json")
                .body(value.to_string()),
            Body::Form(pairs) => builder
                .header("content-type", "application/x-www-form-urlencoded")
                .body(
                    url::form_urlencoded::Serializer::new(String::new())
                        .extend_pairs(pairs)
                        .finish(),
                ),
        };
        let response = builder
            .send()
            .await
            .map_err(|error| Stop::Fail(format!("{} {}: {error}", req.method, req.path)))?;
        let reply = Reply::read(response)
            .await
            .map_err(|error| Stop::Fail(format!("{} {}: {error}", req.method, req.path)))?;
        if req.base == Base::Api
            && let Some(schemas) = &self.schemas
        {
            let body = if reply.is_json {
                ResponseBody::Json(&reply.body)
            } else if reply.body.is_null() {
                ResponseBody::Empty
            } else {
                ResponseBody::Other
            };
            let finding = schemas.check(req.method.as_str(), req.path_only(), reply.status, body);
            if !finding.violations.is_empty() || finding.note.is_some() {
                self.schema_findings
                    .push((format!("{} {}", req.method, req.path), finding));
            }
        }
        Ok(reply)
    }
    /// The target's current time, from a `Date` header (unrecorded).
    pub async fn server_time(&mut self) -> Result<i64, Stop> {
        let path = format!("/users/{}", self.world.invitee);
        let token = self.world.invitee_token.clone();
        let reply = self.call(Req::get(path).bearer(token)).await?;
        match reply.date() {
            Some(now) => Ok(now),
            None => fail("the target sent no Date header"),
        }
    }

    /// A JWT over `header` and `claims`, signed RS256 with `pem`.
    pub fn sign_with(pem: &str, header: &Value, claims: &Value) -> Result<String, Stop> {
        let key = rsa::RsaPrivateKey::from_pkcs8_pem(pem)
            .or_else(|_| rsa::RsaPrivateKey::from_pkcs1_pem(pem))
            .map_err(|error| Stop::Fail(format!("the App private key: {error}")))?;
        let input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let signature = SigningKey::<sha2::Sha256>::new(key)
            .sign(input.as_bytes())
            .to_vec();
        Ok(format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature)))
    }

    /// The App's `iss`: its client ID, else its ID.
    pub fn iss(&self) -> Value {
        match &self.world.app.client_id {
            Some(client_id) => json!(client_id),
            None => json!(self.world.app.id),
        }
    }

    /// `claims` signed with the App's key.
    pub fn app_jwt_with(&self, claims: &Value) -> Result<String, Stop> {
        Self::sign_with(
            &self.world.app.private_key,
            &json!({"alg": "RS256", "typ": "JWT"}),
            claims,
        )
    }

    /// A JWT as GitHub's docs recommend (`iat` 60 s back, `exp` 9 minutes
    /// ahead of the target's clock).
    pub async fn app_jwt(&mut self) -> Result<String, Stop> {
        let now = self.server_time().await?;
        let iss = self.iss();
        self.app_jwt_with(&json!({"iat": now - 60, "exp": now + 540, "iss": iss}))
    }

    /// A new installation token for `installation_id` (unrecorded).
    pub async fn token_for(&mut self, installation_id: u64) -> Result<String, Stop> {
        let jwt = self.app_jwt().await?;
        let reply = self
            .call(
                Req::post(format!(
                    "/app/installations/{installation_id}/access_tokens"
                ))
                .bearer(jwt),
            )
            .await?;
        match (reply.status, reply.str("token")) {
            (201, Some(token)) => Ok(token.to_owned()),
            (status, _) => fail(format!(
                "minting a token for installation {installation_id}: {status} {}",
                reply.body
            )),
        }
    }

    /// A new token for the world's installation (unrecorded).
    pub async fn token(&mut self) -> Result<String, Stop> {
        let id = self.world.installation_id;
        self.token_for(id).await
    }

    /// Revokes a token (unrecorded; the answer does not matter).
    pub async fn revoke(&mut self, token: &str) {
        let _ = self
            .call(Req::delete("/installation/token").bearer(token.to_owned()))
            .await;
    }

    /// Lets `by` pass on the target's clock. The emulator's virtual clock
    /// jumps (the one admin call a scenario may make); a live run waits,
    /// only when slow scenarios are on and for at most 15 minutes, and
    /// skips otherwise.
    pub async fn advance_clock(&mut self, by: Duration) -> Result<(), Stop> {
        if let Some(emulator) = &self.emulator {
            emulator.advance_clock(by);
            return Ok(());
        }
        if !self.slow {
            return skip(format!(
                "needs {} s of real time: live-only/slow (OCTOEMU_LIVE_SLOW=1)",
                by.as_secs()
            ));
        }
        if by > Duration::from_secs(15 * 60) {
            return skip(format!(
                "needs {} s of real time, more than one run waits: run it by hand",
                by.as_secs()
            ));
        }
        tokio::time::sleep(by).await;
        Ok(())
    }

    /// Waits between polls: real time on both targets.
    pub async fn pause(&self, by: Duration) {
        tokio::time::sleep(by).await;
    }

    /// The HTTP client, for scenarios that fan out (the secondary limit).
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }
}

/// The normaliser for a world: its names, IDs and origins.
pub fn normaliser_for(world: &World) -> Normaliser {
    let known = &world.known;
    let mut normaliser = Normaliser::new()
        .origin(&world.api_base)
        .origin(&world.web_base)
        .name(&world.org, "org")
        .name(&world.repo, "repo")
        .name(&world.team, "team")
        .name(&world.invitee, "invitee")
        .name(&known.app_slug, "app")
        .id(world.app.id, "app-id")
        .id(world.installation_id, "installation-id")
        .id(known.org_id, "org-id")
        .id(known.repo_id, "repo-id")
        .id(known.team_id, "team-id")
        .id(known.invitee_id, "invitee-id");
    if let Some(client_id) = &world.app.client_id {
        normaliser = normaliser.name(client_id, "app-client-id");
    }
    normaliser = normaliser.name(&world.signin.client_id, "signin-client-id");
    if let Some(bot) = known.app_bot_id {
        normaliser = normaliser.id(bot, "app-bot-id");
    }
    for extra in [&world.policy, &world.no_seat].into_iter().flatten() {
        normaliser = normaliser
            .name(&extra.account, "extra-org")
            .id(extra.installation_id, "extra-installation-id");
        if let Some(repo) = &extra.repo {
            normaliser = normaliser.name(repo, "extra-repo");
        }
    }
    normaliser
}
