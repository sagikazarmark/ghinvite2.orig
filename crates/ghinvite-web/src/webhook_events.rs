//! Webhook authentication and the installation event decoders (moved from
//! gh-toolkit's `webhook` module, GM-06).
//!
//! Only the exact body is signed. The delivery ID and event-name headers are
//! hints, never authorization evidence: the decoders reject a body whose keys
//! belong to another event kind. The body's SHA-256 is the dedupe key
//! (`webhook/{hex}` in Restate state, `payload_sha256` in
//! `organization_webhook_receipts`); identical legitimate repeats can occur,
//! so correctness still rests on idempotency and reconciliation.
use axum::http::HeaderMap;
use ghinvite_config::apps::WebhookSecrets;
use octoevents::{
    Bytes, Envelope, EventKind, ReceiveError, SignatureError, Verifier, WebhookSecret,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// A body that verified under the app's webhook secrets.
pub(crate) struct Delivery {
    /// The verified body, with the unsigned header meta beside it.
    pub(crate) envelope: Envelope,
    /// SHA-256 of the verified body: the dedupe key.
    pub(crate) payload_sha256: [u8; 32],
}

// Payloads may contain private user data; diagnostics omit them.
impl std::fmt::Debug for Delivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Delivery").finish_non_exhaustive()
    }
}

/// Why a request was not authenticated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rejection {
    /// No configured secret for the app.
    UnknownApp,
    /// The signature header was absent.
    MissingSignature,
    /// The signature was malformed or did not match any secret.
    BadSignature,
    /// The delivery ID or event-name header was absent or empty.
    MissingHeader,
    /// Anything else, such as a non-JSON content type.
    Envelope,
}

impl Rejection {
    /// gh-toolkit's stable rejection code, kept for the `webhook_rejected`
    /// tracing event.
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::UnknownApp => "gh:webhook:unknown_app",
            Self::MissingSignature | Self::MissingHeader => "gh:webhook:missing_header",
            Self::BadSignature => "gh:webhook:bad_signature",
            Self::Envelope => "gh:webhook:decode",
        }
    }
}

/// A verifier accepting every configured secret, for rotation.
pub(crate) fn verifier(secrets: &WebhookSecrets) -> Option<Verifier> {
    let mut secrets = secrets
        .iter()
        .filter_map(|secret| WebhookSecret::try_from(secret.to_vec()).ok());
    let first = secrets.next()?;
    Some(secrets.fold(Verifier::new(first), Verifier::also))
}

/// Authenticate `body` for `app` with `octoevents::authenticate`. Emits the
/// `webhook_signature_failure` (warn) and `webhook_rejected` (info) tracing
/// events gh-toolkit's `TracingSink` emitted for the same failures.
pub(crate) fn authenticate(
    app: &'static str,
    secrets: Option<&WebhookSecrets>,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<Delivery, Rejection> {
    let result = secrets
        .and_then(verifier)
        .ok_or(Rejection::UnknownApp)
        .and_then(|verifier| {
            let payload_sha256 = Sha256::digest(&body).into();
            octoevents::authenticate(&verifier, headers, body)
                .map(|envelope| Delivery {
                    envelope,
                    payload_sha256,
                })
                .map_err(|error| match error {
                    ReceiveError::Signature(error) => {
                        let missing = matches!(error, SignatureError::Missing);
                        signature_failure(app, if missing { "missing" } else { "invalid" });
                        if missing {
                            Rejection::MissingSignature
                        } else {
                            Rejection::BadSignature
                        }
                    }
                    ReceiveError::UnknownTarget => Rejection::UnknownApp,
                    ReceiveError::MissingHeader { .. } => Rejection::MissingHeader,
                    _ => Rejection::Envelope,
                })
        });
    if let Err(rejection) = result {
        tracing::info!(event = "webhook_rejected", app, reason = rejection.code());
    }
    result
}

/// The `webhook_signature_failure` operational signal: `reason` is
/// `missing` or `invalid`.
pub(crate) fn signature_failure(app: &'static str, reason: &'static str) {
    tracing::warn!(
        event = "webhook_signature_failure",
        app,
        reason,
        occurred_at = chrono::Utc::now().timestamp()
    );
}

/// The installation lifecycle actions ghinvite handles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstallationAction {
    Created,
    Deleted,
    Suspend,
    Unsuspend,
    NewPermissionsAccepted,
}

/// Changes to an installation's selected repositories.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RepositoriesAction {
    Added,
    Removed,
}

/// The actor named by the signed body.
#[derive(Clone, Debug)]
pub(crate) struct Sender {
    pub(crate) id: u64,
    pub(crate) login: String,
}

/// A verified `installation` event.
#[derive(Clone, Debug)]
pub(crate) struct InstallationEvent {
    pub(crate) action: InstallationAction,
    pub(crate) sender: Sender,
}

/// A verified `installation_repositories` event.
#[derive(Clone, Debug)]
pub(crate) struct InstallationRepositoriesEvent {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the route refreshes either way")
    )]
    pub(crate) action: RepositoriesAction,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the route refreshes either way")
    )]
    pub(crate) sender: Sender,
}

/// Why a verified body is not the event its header names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KindMismatch;

#[derive(Deserialize)]
struct WireSender {
    id: u64,
    login: String,
}
impl From<WireSender> for Sender {
    fn from(v: WireSender) -> Self {
        Self {
            id: v.id,
            login: v.login,
        }
    }
}
// Decoded for its shape only, as gh-toolkit did: an installation event
// without these fields is not one.
#[derive(Deserialize)]
#[expect(dead_code, reason = "decoded to validate the payload's shape")]
struct WireInstallation {
    id: u64,
    account: WireSender,
    permissions: BTreeMap<String, String>,
}
#[derive(Deserialize)]
#[expect(dead_code, reason = "decoded to validate the payload's shape")]
struct WireRepository {
    id: u64,
    name: String,
    full_name: String,
}
#[derive(Deserialize)]
struct WireEvent {
    action: String,
    #[expect(dead_code, reason = "decoded to validate the payload's shape")]
    installation: WireInstallation,
    sender: WireSender,
}
#[derive(Deserialize)]
struct WireRepositories {
    action: String,
    #[expect(dead_code, reason = "decoded to validate the payload's shape")]
    installation: WireInstallation,
    sender: WireSender,
    #[expect(dead_code, reason = "decoded to validate the payload's shape")]
    repositories_added: Vec<WireRepository>,
    #[expect(dead_code, reason = "decoded to validate the payload's shape")]
    repositories_removed: Vec<WireRepository>,
}

/// Keys of other event kinds' payloads. GitHub signs the body only, so a
/// captured signed delivery can be resent under another event name; a body
/// carrying any of these is not an installation event.
const FOREIGN_KEYS: [&str; 13] = [
    "pull_request",
    "review",
    "check_run",
    "check_suite",
    "team",
    "member",
    "ref",
    "before",
    "after",
    "sha",
    "state",
    "context",
    "repository",
];

fn validate(
    delivery: &Delivery,
    kind: EventKind,
    repository_change: bool,
) -> Result<(), KindMismatch> {
    if delivery.envelope.meta.kind != kind {
        return Err(KindMismatch);
    }
    let body: serde_json::Value =
        serde_json::from_slice(&delivery.envelope.raw_payload).map_err(|_| KindMismatch)?;
    let object = body.as_object().ok_or(KindMismatch)?;
    if FOREIGN_KEYS.iter().any(|key| object.contains_key(*key)) {
        return Err(KindMismatch);
    }
    if !repository_change
        && (object.contains_key("repositories_added")
            || object.contains_key("repositories_removed"))
    {
        return Err(KindMismatch);
    }
    Ok(())
}

impl InstallationEvent {
    /// Decode an `installation` event, rejecting a body of another kind.
    pub(crate) fn decode(delivery: &Delivery) -> Result<Self, KindMismatch> {
        validate(delivery, EventKind::Installation, false)?;
        let v: WireEvent =
            serde_json::from_slice(&delivery.envelope.raw_payload).map_err(|_| KindMismatch)?;
        let action = match v.action.as_str() {
            "created" => InstallationAction::Created,
            "deleted" => InstallationAction::Deleted,
            "suspend" => InstallationAction::Suspend,
            "unsuspend" => InstallationAction::Unsuspend,
            "new_permissions_accepted" => InstallationAction::NewPermissionsAccepted,
            _ => return Err(KindMismatch),
        };
        Ok(Self {
            action,
            sender: v.sender.into(),
        })
    }
}

impl InstallationRepositoriesEvent {
    /// Decode an `installation_repositories` event, rejecting a body of
    /// another kind.
    pub(crate) fn decode(delivery: &Delivery) -> Result<Self, KindMismatch> {
        validate(delivery, EventKind::InstallationRepositories, true)?;
        let v: WireRepositories =
            serde_json::from_slice(&delivery.envelope.raw_payload).map_err(|_| KindMismatch)?;
        let action = match v.action.as_str() {
            "added" => RepositoriesAction::Added,
            "removed" => RepositoriesAction::Removed,
            _ => return Err(KindMismatch),
        };
        Ok(Self {
            action,
            sender: v.sender.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use octoevents::{WebhookMeta, header};
    use serde_json::{Value, json};

    fn delivery(kind: EventKind, body: &Value) -> Delivery {
        let body = serde_json::to_vec(body).unwrap();
        Delivery {
            payload_sha256: Sha256::digest(&body).into(),
            envelope: Envelope::new(WebhookMeta::new("d", kind), body),
        }
    }
    fn installation(action: &str) -> Value {
        json!({"action":action,"installation":{"id":1,"account":{"id":100,"login":"acme"},
            "permissions":{"metadata":"read"}},"sender":{"id":7,"login":"octocat"}})
    }
    fn repositories(action: &str) -> Value {
        let mut body = installation(action);
        body["repositories_added"] = json!([{"id":10,"name":"api","full_name":"acme/api"}]);
        body["repositories_removed"] = json!([]);
        body
    }
    fn secrets(input: &str) -> WebhookSecrets {
        WebhookSecrets::parse(input).unwrap()
    }
    fn signed(secret: &str, body: &'static [u8]) -> HeaderMap {
        let signer = Verifier::new(WebhookSecret::try_from(secret.as_bytes().to_vec()).unwrap());
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        headers.insert(header::DELIVERY_ID, "d".parse().unwrap());
        headers.insert(header::EVENT_NAME, "installation".parse().unwrap());
        headers.insert(header::SIGNATURE, signer.sign(body).into());
        headers
    }

    #[test]
    fn every_configured_secret_verifies_and_the_digest_is_of_the_body() {
        const BODY: &[u8] = br#"{"action":"created"}"#;
        let configured = secrets("new-secret;old-secret");
        for secret in ["new-secret", "old-secret"] {
            let delivery = authenticate(
                "repo",
                Some(&configured),
                &signed(secret, BODY),
                Bytes::from_static(BODY),
            )
            .unwrap();
            assert_eq!(
                delivery.payload_sha256,
                <[u8; 32]>::from(Sha256::digest(BODY))
            );
            assert_eq!(delivery.envelope.raw_payload, BODY);
            assert_eq!(delivery.envelope.meta.kind, EventKind::Installation);
        }
        assert_eq!(
            authenticate(
                "repo",
                Some(&configured),
                &signed("retired-secret", BODY),
                Bytes::from_static(BODY)
            )
            .unwrap_err(),
            Rejection::BadSignature
        );
        assert_eq!(
            authenticate(
                "repo",
                None,
                &signed("new-secret", BODY),
                Bytes::from_static(BODY)
            )
            .unwrap_err(),
            Rejection::UnknownApp
        );
    }

    #[test]
    fn authentication_failures_are_classified() {
        const BODY: &[u8] = br#"{"action":"created"}"#;
        let configured = secrets("secret");
        let check = |edit: &dyn Fn(&mut HeaderMap), want: Rejection| {
            let mut headers = signed("secret", BODY);
            edit(&mut headers);
            assert_eq!(
                authenticate(
                    "repo",
                    Some(&configured),
                    &headers,
                    Bytes::from_static(BODY)
                )
                .unwrap_err(),
                want
            );
        };
        check(
            &|h| {
                h.remove(header::SIGNATURE);
            },
            Rejection::MissingSignature,
        );
        check(
            &|h| {
                h.insert(header::SIGNATURE, "sha256=zz".parse().unwrap());
            },
            Rejection::BadSignature,
        );
        check(
            &|h| {
                h.insert(header::EVENT_NAME, "".parse().unwrap());
            },
            Rejection::MissingHeader,
        );
        check(
            &|h| {
                h.remove(header::DELIVERY_ID);
            },
            Rejection::MissingHeader,
        );
        check(
            &|h| {
                h.insert(header::CONTENT_TYPE, "text/plain".parse().unwrap());
            },
            Rejection::Envelope,
        );
    }

    #[test]
    fn installation_events_decode_their_action_and_sender() {
        for (action, want) in [
            ("created", InstallationAction::Created),
            ("deleted", InstallationAction::Deleted),
            ("suspend", InstallationAction::Suspend),
            ("unsuspend", InstallationAction::Unsuspend),
            (
                "new_permissions_accepted",
                InstallationAction::NewPermissionsAccepted,
            ),
        ] {
            let event = InstallationEvent::decode(&delivery(
                EventKind::Installation,
                &installation(action),
            ))
            .unwrap();
            assert_eq!(event.action, want);
            assert_eq!(
                (event.sender.id, event.sender.login.as_str()),
                (7, "octocat")
            );
        }
        for (action, want) in [
            ("added", RepositoriesAction::Added),
            ("removed", RepositoriesAction::Removed),
        ] {
            let event = InstallationRepositoriesEvent::decode(&delivery(
                EventKind::InstallationRepositories,
                &repositories(action),
            ))
            .unwrap();
            assert_eq!(event.action, want);
            assert_eq!(event.sender.id, 7);
        }
    }

    #[test]
    fn bodies_of_another_kind_are_rejected() {
        let check = |kind: EventKind, body: Value| {
            let delivery = delivery(kind.clone(), &body);
            if kind == EventKind::InstallationRepositories {
                InstallationRepositoriesEvent::decode(&delivery).map(drop)
            } else {
                InstallationEvent::decode(&delivery).map(drop)
            }
        };
        for key in FOREIGN_KEYS {
            let mut body = installation("deleted");
            body[key] = json!({});
            assert_eq!(
                check(EventKind::Installation, body),
                Err(KindMismatch),
                "{key}"
            );
            let mut body = repositories("added");
            body[key] = json!(null);
            assert_eq!(
                check(EventKind::InstallationRepositories, body),
                Err(KindMismatch),
                "{key}"
            );
        }
        for key in ["repositories_added", "repositories_removed"] {
            let mut body = installation("created");
            body[key] = json!([]);
            assert_eq!(check(EventKind::Installation, body), Err(KindMismatch));
        }
        // The header's kind must be the decoder's.
        assert_eq!(
            InstallationEvent::decode(&delivery(
                EventKind::InstallationRepositories,
                &installation("created")
            ))
            .map(drop),
            Err(KindMismatch)
        );
        // Unknown actions, missing fields and non-objects are not the event.
        let mut no_sender = installation("created");
        no_sender.as_object_mut().unwrap().remove("sender");
        let mut no_permissions = installation("created");
        no_permissions["installation"]
            .as_object_mut()
            .unwrap()
            .remove("permissions");
        for body in [
            installation("renamed"),
            no_sender,
            no_permissions,
            json!([]),
        ] {
            assert_eq!(check(EventKind::Installation, body), Err(KindMismatch));
        }
        let mut no_removed = repositories("added");
        no_removed
            .as_object_mut()
            .unwrap()
            .remove("repositories_removed");
        for body in [repositories("edited"), no_removed] {
            assert_eq!(
                check(EventKind::InstallationRepositories, body),
                Err(KindMismatch)
            );
        }
    }
}
