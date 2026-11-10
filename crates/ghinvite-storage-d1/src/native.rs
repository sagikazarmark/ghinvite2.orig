//! Native transport facade for the private same-authority D1 binding bridge.
//! This is deliberately not a second implementation of the storage traits.
use reqwest::{
    Client, Url,
    header::{AUTHORIZATION, HeaderValue},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{sync::Arc, time::Duration};

const MAX_BODY: usize = 32 * 1024 * 1024;
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Error(&'static str);
const UNAVAILABLE: Error = Error("D1 bridge unavailable");
#[derive(Clone, Serialize)]
#[serde(transparent)]
pub struct JsValue(serde_json::Value);
impl JsValue {
    pub const NULL: Self = Self(serde_json::Value::Null);
    // Compatibility facade for wasm_bindgen::JsValue: this constructor is infallible.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(v: &str) -> Self {
        Self(serde_json::Value::String(v.into()))
    }
    pub fn from_f64(v: f64) -> Self {
        if v.is_finite() && v.fract() == 0.0 && v.abs() <= 9007199254740991.0 {
            Self(serde_json::Value::Number(serde_json::Number::from(
                v as i64,
            )))
        } else {
            Self(serde_json::Value::Bool(false))
        }
    }
}
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Migration {
    pub name: String,
    pub sha256: String,
}
/// Deployment-produced identity, not an authority record created by this adapter.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthorityManifest {
    pub protocol: u32,
    pub database_id: String,
    pub migrations: Vec<Migration>,
    pub schema_sha256: String,
}
impl AuthorityManifest {
    fn valid(&self) -> bool {
        let hex = |s: &str| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        };
        self.protocol == 1
            && self.database_id.len() == 36
            && self.database_id.bytes().enumerate().all(|(i, b)| {
                if [8, 13, 18, 23].contains(&i) {
                    b == b'-'
                } else {
                    b.is_ascii_hexdigit()
                }
            })
            && hex(&self.schema_sha256)
            && !self.migrations.is_empty()
            && self.migrations.iter().all(|m| {
                hex(&m.sha256) && m.name.ends_with(".sql") && !m.name.contains(['/', '\\'])
            })
            && self.migrations.windows(2).all(|w| w[0].name < w[1].name)
    }
}
/// No Debug implementation: even a diagnostic cannot print the credential.
pub struct BridgeConfig {
    url: Url,
    token: HeaderValue,
    manifest: AuthorityManifest,
}
impl BridgeConfig {
    pub fn new(url: &str, token: String, manifest: AuthorityManifest) -> Result<Self, Error> {
        Self::parse(url, token, manifest, false)
    }
    #[cfg(feature = "bridge-test")]
    pub fn local_for_test(
        url: &str,
        token: String,
        manifest: AuthorityManifest,
    ) -> Result<Self, Error> {
        Self::parse(url, token, manifest, true)
    }
    fn parse(
        url: &str,
        token: String,
        manifest: AuthorityManifest,
        local: bool,
    ) -> Result<Self, Error> {
        let url = Url::parse(url).map_err(|_| UNAVAILABLE)?;
        let secure = url.scheme() == "https";
        let loopback = local
            && url.scheme() == "http"
            && matches!(url.host_str(), Some("127.0.0.1" | "[::1]"));
        if !(secure || loopback)
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/v1/d1"
            || token.len() != 64
            || !token
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || !manifest.valid()
        {
            return Err(UNAVAILABLE);
        }
        let mut token =
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| UNAVAILABLE)?;
        token.set_sensitive(true);
        Ok(Self {
            url,
            token,
            manifest,
        })
    }
}
struct Inner {
    client: Client,
    config: BridgeConfig,
}
#[derive(Clone)]
pub struct D1Database(Arc<Inner>);
#[derive(Clone, Copy)]
pub enum D1SessionConstraint {
    FirstPrimary,
}
#[derive(Clone, Serialize)]
struct Query {
    sql: String,
    params: Vec<JsValue>,
}
#[derive(Clone)]
pub struct D1PreparedStatement {
    db: D1Database,
    query: Query,
}
#[derive(Deserialize)]
struct Reply {
    protocol: u32,
    authority: AuthorityManifest,
    results: Vec<RawResult>,
    error: Option<String>,
}
#[derive(Clone, Deserialize)]
struct RawResult {
    success: bool,
    results: Vec<serde_json::Value>,
    meta: Meta,
}
#[derive(Clone, Deserialize)]
pub struct Meta {
    pub changes: Option<usize>,
}
pub struct D1Result(RawResult);
impl D1Database {
    /// Authenticated read-only readiness, before a native endpoint is opened.
    pub async fn connect(config: BridgeConfig) -> Result<Self, Error> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(15))
            .no_proxy()
            .build()
            .map_err(|_| UNAVAILABLE)?;
        let db = Self(Arc::new(Inner { client, config }));
        db.execute(vec![]).await?;
        Ok(db)
    }
    pub fn prepare(&self, sql: &str) -> D1PreparedStatement {
        D1PreparedStatement {
            db: self.clone(),
            query: Query {
                sql: sql.into(),
                params: vec![],
            },
        }
    }
    pub fn with_session_constraint(&self, _: D1SessionConstraint) -> Result<Self, Error> {
        Ok(self.clone())
    }
    pub async fn batch(
        &self,
        statements: Vec<D1PreparedStatement>,
    ) -> Result<Vec<D1Result>, Error> {
        if statements.iter().any(|s| !Arc::ptr_eq(&s.db.0, &self.0)) {
            return Err(UNAVAILABLE);
        }
        self.execute(statements.into_iter().map(|s| s.query).collect())
            .await
    }
    async fn execute(&self, queries: Vec<Query>) -> Result<Vec<D1Result>, Error> {
        // One attempt only: lost responses remain uncertain for the retained owners.
        let count = queries.len();
        let body = serde_json::to_vec(
            &serde_json::json!({"protocol":1,"authority":self.0.config.manifest,"batch":queries}),
        )
        .map_err(|_| UNAVAILABLE)?;
        if body.len() > MAX_BODY || count > 100 {
            return Err(UNAVAILABLE);
        }
        let mut response = self
            .0
            .client
            .post(self.0.config.url.clone())
            .header(AUTHORIZATION, self.0.config.token.clone())
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| UNAVAILABLE)?;
        if response.status() != reqwest::StatusCode::OK
            || response
                .content_length()
                .is_some_and(|n| n > MAX_BODY as u64)
        {
            return Err(UNAVAILABLE);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| UNAVAILABLE)? {
            if bytes.len().saturating_add(chunk.len()) > MAX_BODY {
                return Err(UNAVAILABLE);
            }
            bytes.extend_from_slice(&chunk);
        }
        let reply: Reply = serde_json::from_slice(&bytes).map_err(|_| UNAVAILABLE)?;
        if reply.protocol != 1
            || reply.authority != self.0.config.manifest
            || (reply.error.is_none()
                && (reply.results.len() != count
                    || reply
                        .results
                        .iter()
                        .any(|r| !r.success || r.meta.changes.is_none())))
        {
            return Err(UNAVAILABLE);
        }
        if let Some(code) = reply.error {
            return Err(Error(match code.as_str() {
                "dependency" => "projection_dependency",
                "invariant" => "projection_invariant",
                "changed" => "projection_changed",
                "unique_account" => "UNIQUE constraint failed: installations.account_id",
                "unique" => "UNIQUE constraint failed",
                "foreign_key" => "FOREIGN KEY constraint failed",
                _ => "D1 bridge unavailable",
            }));
        }
        Ok(reply.results.into_iter().map(D1Result).collect())
    }
}
impl D1PreparedStatement {
    pub fn bind(mut self, params: &[JsValue]) -> Result<Self, Error> {
        if params.iter().any(|v| {
            !matches!(
                &v.0,
                serde_json::Value::String(_)
                    | serde_json::Value::Null
                    | serde_json::Value::Number(_)
            )
        }) {
            return Err(UNAVAILABLE);
        }
        self.query.params = params.to_vec();
        Ok(self)
    }
    pub async fn all(self) -> Result<D1Result, Error> {
        let db = self.db.clone();
        db.batch(vec![self]).await?.pop().ok_or(UNAVAILABLE)
    }
    pub async fn run(self) -> Result<D1Result, Error> {
        self.all().await
    }
    pub async fn first<T: DeserializeOwned>(
        self,
        column: Option<&str>,
    ) -> Result<Option<T>, Error> {
        if column.is_some() {
            return Err(UNAVAILABLE);
        }
        self.all()
            .await?
            .0
            .results
            .into_iter()
            .next()
            .map(|v| serde_json::from_value(v).map_err(|_| UNAVAILABLE))
            .transpose()
    }
}
impl D1Result {
    pub fn results<T: DeserializeOwned>(&self) -> Result<Vec<T>, Error> {
        self.0
            .results
            .iter()
            .cloned()
            .map(|v| serde_json::from_value(v).map_err(|_| UNAVAILABLE))
            .collect()
    }
    pub fn meta(&self) -> Result<Option<Meta>, Error> {
        Ok(Some(self.0.meta.clone()))
    }
}
