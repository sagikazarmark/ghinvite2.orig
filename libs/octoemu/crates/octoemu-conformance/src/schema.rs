//! The schema check: every emulator response is validated against the
//! response schemas of GitHub's OpenAPI 3.1 description, pinned and cut down
//! to the operations octoemu implements (`schemas/rest-api-subset.json`,
//! written by `schemas/extract.py`). Known errors in the published schemas
//! are documented exceptions in `schemas/exceptions.toml`.
//!
//! This checks shape; the scenarios check behaviour.

use jsonschema::{Draft, Validator};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

const SUBSET: &str = include_str!("../schemas/rest-api-subset.json");
const EXCEPTIONS: &str = include_str!("../schemas/exceptions.toml");

/// A documented error in the published description.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exception {
    /// `METHOD /pattern`.
    pub route: String,
    /// The response status, as the description keys it (`"200"`).
    pub status: String,
    /// The JSON pointer of the instance location (and everything under it)
    /// whose violations are excused; `""` for the whole body.
    pub pointer: String,
    /// Why the published schema is wrong there.
    pub reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExceptionFile {
    #[serde(default)]
    exception: Vec<Exception>,
}

/// A response body, as the check sees it.
#[derive(Clone, Copy, Debug)]
pub enum ResponseBody<'a> {
    /// None at all.
    Empty,
    /// JSON.
    Json(&'a Value),
    /// Something that is not JSON.
    Other,
}

/// What the check found for one response.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Finding {
    /// The operation matched (`GET /app`), if any.
    pub route: Option<String>,
    /// Schema violations: `instance pointer: message`.
    pub violations: Vec<String>,
    /// Why nothing (or only the generic error schema) was checked.
    pub note: Option<String>,
}

/// The pinned schemas.
pub struct Schemas {
    subset: Value,
    routes: Vec<(String, String, Vec<String>)>,
    exceptions: Vec<Exception>,
    validators: Mutex<HashMap<(String, String), Arc<Validator>>>,
}

impl std::fmt::Debug for Schemas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Schemas")
            .field("source", &self.subset["source"])
            .finish_non_exhaustive()
    }
}

impl Schemas {
    /// The checked-in subset and exceptions.
    pub fn embedded() -> Result<Self, String> {
        let subset: Value = serde_json::from_str(SUBSET)
            .map_err(|error| format!("rest-api-subset.json: {error}"))?;
        let ExceptionFile { exception } =
            toml::from_str(EXCEPTIONS).map_err(|error| format!("exceptions.toml: {error}"))?;
        let routes = subset["operations"]
            .as_object()
            .ok_or("rest-api-subset.json: no operations")?
            .keys()
            .filter_map(|route| {
                let (method, path) = route.split_once(' ')?;
                Some((
                    route.clone(),
                    method.to_owned(),
                    path.trim_start_matches('/')
                        .split('/')
                        .map(str::to_owned)
                        .collect(),
                ))
            })
            .collect();
        Ok(Self {
            subset,
            routes,
            exceptions: exception,
            validators: Mutex::new(HashMap::new()),
        })
    }

    /// The pin: `{repository, commit, path, sha256, …}`.
    pub fn source(&self) -> &Value {
        &self.subset["source"]
    }

    /// Every operation in the subset, `METHOD /pattern`.
    pub fn operations(&self) -> impl Iterator<Item = &str> {
        self.routes.iter().map(|(route, _, _)| route.as_str())
    }

    /// Routes octoemu implements that the description does not have.
    pub fn undocumented(&self) -> Vec<&str> {
        self.subset["undocumented"]
            .as_array()
            .map(|routes| routes.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    }

    /// The documented exceptions.
    pub fn exceptions(&self) -> &[Exception] {
        &self.exceptions
    }

    /// The operation `method path` (no query) falls under: the template
    /// with the most literal segments among those that match.
    pub fn route_of(&self, method: &str, path: &str) -> Option<&str> {
        let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        self.routes
            .iter()
            .filter(|(_, m, template)| m == method && template.len() == segments.len())
            .filter_map(|(route, _, template)| {
                let mut literal = 0;
                for (want, got) in template.iter().zip(&segments) {
                    if want.starts_with('{') && want.ends_with('}') {
                        if got.is_empty() {
                            return None;
                        }
                    } else if want == got {
                        literal += 1;
                    } else {
                        return None;
                    }
                }
                Some((literal, route.as_str()))
            })
            .max_by_key(|(literal, _)| *literal)
            .map(|(_, route)| route)
    }

    fn validator(
        &self,
        route: &str,
        status: &str,
        schema: &Value,
    ) -> Result<Arc<Validator>, String> {
        let key = (route.to_owned(), status.to_owned());
        let mut cache = self
            .validators
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(validator) = cache.get(&key) {
            return Ok(validator.clone());
        }
        // The response schema as the root, with the components beside it
        // so `#/components/schemas/…` resolves.
        let mut root = match schema {
            Value::Object(object) => object.clone(),
            other => {
                let mut object = Map::new();
                object.insert("allOf".into(), Value::Array(vec![other.clone()]));
                object
            }
        };
        root.insert("components".into(), self.subset["components"].clone());
        let validator = jsonschema::options()
            .with_draft(Draft::Draft202012)
            .build(&Value::Object(root))
            .map_err(|error| format!("{route} {status}: the schema does not compile: {error}"))?;
        let validator = Arc::new(validator);
        cache.insert(key, validator.clone());
        Ok(validator)
    }

    /// Checks one response. Violation messages name the operation, the
    /// instance pointer and the failed keyword, never the response's values
    /// (a live response may hold addresses or tokens).
    pub fn check(&self, method: &str, path: &str, status: u16, body: ResponseBody<'_>) -> Finding {
        let mut finding = Finding::default();
        let Some(route) = self.route_of(method, path) else {
            finding.note = Some(format!("{method} {path}: no operation in the description"));
            return finding;
        };
        finding.route = Some(route.to_owned());
        let responses = &self.subset["operations"][route]["responses"];
        let code = status.to_string();
        let (key, schema) = if let Some(schema) = responses.get(&code) {
            (code.clone(), schema.clone())
        } else if let Some(schema) = responses.get("default") {
            ("default".to_owned(), schema.clone())
        } else if status >= 400 {
            finding.note = Some(format!(
                "{route}: status {status} is not in the description; checked as basic-error"
            ));
            (
                code.clone(),
                serde_json::json!({"$ref": "#/components/schemas/basic-error"}),
            )
        } else {
            // A success the description does not list: nothing checks the
            // body, so the shape is unknown.
            finding.violations.push(format!(
                "{route}: status {status} is not in the description"
            ));
            return finding;
        };
        match (schema.is_null(), body) {
            (true, ResponseBody::Empty) => {}
            (true, _) => finding.violations.push(format!(
                "{route} {key}: a body where the description has none"
            )),
            (false, ResponseBody::Empty) => finding.violations.push(format!(
                "{route} {key}: no body where the description has a JSON one"
            )),
            (false, ResponseBody::Other) => finding.violations.push(format!(
                "{route} {key}: a body that is not JSON where the description has a JSON one"
            )),
            (false, ResponseBody::Json(body)) => match self.validator(route, &key, &schema) {
                Err(error) => finding.violations.push(error),
                Ok(validator) => {
                    for error in validator.iter_errors(body) {
                        let pointer = error.instance_path().to_string();
                        let excused = self.exceptions.iter().any(|exception| {
                            exception.route == route
                                && exception.status == key
                                && (pointer == exception.pointer
                                    || pointer.starts_with(&format!("{}/", exception.pointer)))
                        });
                        if !excused {
                            finding.violations.push(format!(
                                "{route} {key} at {pointer:?} ({}): {}",
                                error.kind().keyword(),
                                error.masked()
                            ));
                        }
                    }
                }
            },
        }
        finding
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_subset_is_pinned_and_compiles() {
        let schemas = Schemas::embedded().unwrap();
        let source = schemas.source();
        assert_eq!(source["repository"], "github/rest-api-description");
        assert_eq!(source["commit"].as_str().unwrap().len(), 40);
        assert!(source["openapi"].as_str().unwrap().starts_with("3.1"));
        assert_eq!(
            schemas.route_of("GET", "/user/emails"),
            Some("GET /user/emails")
        );
        assert_eq!(
            schemas.route_of("GET", "/user/12"),
            Some("GET /user/{account_id}")
        );
        assert_eq!(schemas.route_of("GET", "/nope"), None);
        // A wrong shape is caught; a right one passes.
        let bad = schemas.check(
            "GET",
            "/user/emails",
            200,
            ResponseBody::Json(&json!({"email": "a@example.com"})),
        );
        assert!(!bad.violations.is_empty());
        // The value never shows in the diagnostic.
        assert!(
            bad.violations.iter().all(|v| !v.contains("a@example.com")),
            "{bad:?}"
        );
        let good = schemas.check(
            "GET",
            "/user/emails",
            200,
            ResponseBody::Json(&json!([{"email": "a@example.com", "primary": true, "verified": true, "visibility": null}])),
        );
        assert!(good.violations.is_empty(), "{good:?}");
        // An undocumented error status falls back to basic-error.
        let error = schemas.check(
            "GET",
            "/app",
            401,
            ResponseBody::Json(&json!({"message": "Bad credentials"})),
        );
        assert!(
            error.violations.is_empty() && error.note.is_some(),
            "{error:?}"
        );
        // An undocumented success, a body where none is documented, text
        // where JSON is: violations.
        let app = json!({"slug": "x"});
        for (status, body) in [
            (201, ResponseBody::Json(&app)),
            (200, ResponseBody::Other),
            (200, ResponseBody::Empty),
        ] {
            let finding = schemas.check("GET", "/app", status, body);
            assert!(!finding.violations.is_empty(), "{status} {body:?}");
        }
        let text = schemas.check("DELETE", "/installation/token", 204, ResponseBody::Other);
        assert!(!text.violations.is_empty());
        let none = schemas.check("DELETE", "/installation/token", 204, ResponseBody::Empty);
        assert!(none.violations.is_empty(), "{none:?}");
        for exception in schemas.exceptions() {
            assert!(!exception.reason.trim().is_empty());
        }
    }

    #[test]
    fn every_rest_route_octoemu_implements_is_in_the_subset() {
        // Regenerate schemas/rest-api-subset.json (schemas/extract.py) when
        // octoemu gains a route.
        let schemas = Schemas::embedded().unwrap();
        let manifest = octoemu::fidelity::Manifest::embedded().unwrap();
        let known: Vec<&str> = schemas.operations().chain(schemas.undocumented()).collect();
        for route in manifest.routes.keys() {
            let path = route.split_once(' ').map_or("", |(_, path)| path);
            if path.starts_with("/login/") || path.starts_with("/avatars/") {
                continue;
            }
            assert!(
                known.contains(&route.as_str()),
                "{route} is not in the schema subset"
            );
        }
    }
}
