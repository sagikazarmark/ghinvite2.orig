//! PD-8: an `installation.deleted` delivery is confirmed with GitHub before
//! the installation is retired for good.
//!
//! GitHub signs the body only, so a captured signed delivery can be resent
//! under another event name or long after it was sent. The app-authenticated
//! installation read is authoritative: a 404 confirms the deletion; an
//! installation GitHub still reports is not retired, and the delivery is
//! handled without effect.
//!
//! # Replay of journals from before this check
//!
//! A handler that meets a journaled step under another name at the same
//! position fails the invocation (journal mismatch). The check is therefore
//! the step that stood here before, `uninstall_time`, which journaled the
//! bare uninstall time as Unix milliseconds. Its result,
//! [`UninstallTime`], decodes that old entry as [`UninstallTime::Unchecked`]:
//! an `installation.deleted` invocation journaled before this deployment and
//! replayed after it retires the installation exactly as it would have, and
//! no drain is needed. A new entry is an object, so the two cannot be
//! confused.
//!
//! `onboard_webhook`'s answer stays compatible the same way: `None` is the
//! empty body its former `()` answered (see [`WebhookOutcome`]).
use super::{AccountInstallation, InstallationReadFailure};
use ghinvite_core::account::App;
use restate_sdk::{
    context::{ContextSideEffects, ObjectContext, RunFuture},
    errors::{HandlerError, TerminalError},
    serde::Json,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The journaled result of the `uninstall_time` step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(super) enum UninstallTime {
    /// Journaled before PD-8: the uninstall time in Unix milliseconds, with no
    /// check. Replayed as a confirmed deletion, as it was then.
    Unchecked(u64),
    /// GitHub's answer to the installation read.
    Checked(DeletionCheck),
}

/// GitHub's answer when asked whether a deleted installation is gone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "installation", rename_all = "snake_case")]
pub(super) enum DeletionCheck {
    /// The read was answered 404 at `at` (Unix milliseconds).
    Gone { at: u64 },
    /// GitHub still reports the installation.
    Exists,
}

impl UninstallTime {
    /// The confirmed uninstall time, or `None` when the installation exists.
    pub(super) fn confirmed_at(
        &self,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, TerminalError> {
        let millis = match self {
            Self::Unchecked(at) | Self::Checked(DeletionCheck::Gone { at }) => *at,
            Self::Checked(DeletionCheck::Exists) => return Ok(None),
        };
        i64::try_from(millis)
            .ok()
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(Some)
            .ok_or_else(|| TerminalError::new("uninstall time out of range"))
    }
}

/// What `AccountInstallation::onboard_webhook` reports to the `Installation`
/// owner. The handler answers `Option<Json<WebhookOutcome>>`: `None`, which
/// Restate carries as an empty body exactly as the handler's former `()`, for
/// everything the owner handled as before, so an `Installation` journal from
/// before PD-8 replays unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WebhookOutcome {
    /// An `installation.deleted` delivery GitHub did not confirm: the
    /// installation is live and must not be marked uninstalled.
    DeletionNotConfirmed,
}

impl AccountInstallation {
    /// Confirm the deletion of `app`'s installation `id`, journaled as
    /// `uninstall_time` (see the module docs). A transient read failure is
    /// retried by Restate as the credential refresh is; any other failure is
    /// terminal, as for the other app-authenticated reads.
    pub(super) async fn confirm_deletion(
        &self,
        ctx: &ObjectContext<'_>,
        app: App,
        id: u64,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, TerminalError> {
        let Json(time) = ctx
            .run(|| async { self.check_deletion(app, id).await.map(Json) })
            .name("uninstall_time")
            .await?;
        time.confirmed_at()
    }

    pub(super) async fn check_deletion(
        &self,
        app: App,
        id: u64,
    ) -> Result<UninstallTime, HandlerError> {
        match self.read_installation(app, id).await {
            Ok(None) => Ok(UninstallTime::Checked(DeletionCheck::Gone {
                at: u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0),
            })),
            Ok(Some(_)) => Ok(UninstallTime::Checked(DeletionCheck::Exists)),
            Err(InstallationReadFailure::Failed { transient: true }) => Err(HandlerError::from(
                std::io::Error::other("installation deletion could not be confirmed"),
            )),
            Err(InstallationReadFailure::Failed { transient: false }) => Err(
                TerminalError::new_with_code(503, "installation deletion could not be confirmed")
                    .into(),
            ),
            Err(InstallationReadFailure::Refused(error)) => Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghinvite_github::{
        AppSlot, GithubApps, Method,
        mocks::{API_BASE as BASE, Expectation, FixedTokens, MockTransport},
    };
    use serde_json::json;

    fn installation(id: u64, app_id: u64) -> serde_json::Value {
        json!({"id":id,"app_id":app_id,"account":{"id":100,"login":"acme","type":"Organization"},
            "created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","suspended_at":null,
            "repository_selection":"all","permissions":{"metadata":"read"}})
    }
    async fn owner(script: Vec<Expectation>) -> (AccountInstallation, MockTransport) {
        let transport = MockTransport::scripted(script);
        let apps = GithubApps::for_tests(
            &transport,
            [
                (AppSlot::Repo, FixedTokens::new(123)),
                (AppSlot::Org, FixedTokens::new(456)),
            ],
        );
        let state = crate::test_support::fixture_state()
            .await
            .with_github_apps(crate::test_github::repository_github(apps));
        (AccountInstallation { state }, transport)
    }
    fn read(id: u64) -> String {
        format!("{BASE}/app/installations/{id}")
    }
    fn message(error: &HandlerError) -> String {
        <HandlerError as AsRef<dyn std::error::Error>>::as_ref(error).to_string()
    }

    #[tokio::test]
    async fn a_deletion_github_confirms_retires_at_the_time_it_was_read() {
        let (owner, transport) = owner(vec![Expectation::status(Method::Get, read(7), 404)]).await;
        let before = chrono::Utc::now();
        let checked = owner.check_deletion(App::Repo, 7).await.unwrap();
        transport.assert_exhausted();
        let at = checked.confirmed_at().unwrap().expect("confirmed");
        assert!(at >= before - chrono::Duration::milliseconds(1) && at <= chrono::Utc::now());
    }

    #[tokio::test]
    async fn a_deletion_of_an_installation_github_still_reports_is_not_confirmed() {
        for (app, app_id) in [(App::Repo, 123), (App::Org, 456)] {
            let (owner, transport) = owner(vec![Expectation::ok_json(
                Method::Get,
                read(7),
                installation(7, app_id),
            )])
            .await;
            let checked = owner.check_deletion(app, 7).await.unwrap();
            transport.assert_exhausted();
            assert_eq!(checked, UninstallTime::Checked(DeletionCheck::Exists));
            assert_eq!(checked.confirmed_at().unwrap(), None);
        }
    }

    #[tokio::test]
    async fn transient_read_failures_are_retried_and_others_are_terminal() {
        for status in [500, 502, 503] {
            let (owner, transport) =
                owner(vec![Expectation::status(Method::Get, read(7), status)]).await;
            let error = owner.check_deletion(App::Repo, 7).await.unwrap_err();
            transport.assert_exhausted();
            assert!(message(&error).starts_with("Retryable error"), "{status}");
        }
        for status in [401, 403, 422] {
            let (owner, _) = owner(vec![Expectation::status(Method::Get, read(7), status)]).await;
            let error = owner.check_deletion(App::Repo, 7).await.unwrap_err();
            assert!(message(&error).starts_with("Terminal error"), "{status}");
        }
        // An answer about another installation or app confirms nothing.
        for body in [installation(8, 123), installation(7, 456)] {
            let (owner, _) = owner(vec![Expectation::ok_json(Method::Get, read(7), body)]).await;
            let error = owner.check_deletion(App::Repo, 7).await.unwrap_err();
            assert!(message(&error).starts_with("Terminal error"));
        }
    }

    /// The journal entries: an `uninstall_time` entry written before PD-8 (a
    /// bare number of milliseconds) still decodes, as a confirmed deletion.
    #[test]
    fn uninstall_time_entries_from_before_and_after_the_check_decode() {
        let decode = |bytes: &[u8]| {
            let mut bytes = bytes::Bytes::copy_from_slice(bytes);
            <Json<UninstallTime> as restate_sdk::serde::Deserialize>::deserialize(&mut bytes)
                .unwrap()
                .into_inner()
        };
        // As restate-ext's clock journals `clock::now(ctx, "uninstall_time")`.
        let legacy = decode(b"1767225600000");
        assert_eq!(legacy, UninstallTime::Unchecked(1_767_225_600_000));
        assert_eq!(
            legacy.confirmed_at().unwrap(),
            Some("2026-01-01T00:00:00Z".parse().unwrap())
        );
        for (value, json) in [
            (
                UninstallTime::Checked(DeletionCheck::Gone {
                    at: 1_767_225_600_000,
                }),
                r#"{"installation":"gone","at":1767225600000}"#,
            ),
            (
                UninstallTime::Checked(DeletionCheck::Exists),
                r#"{"installation":"exists"}"#,
            ),
        ] {
            let encoded = <Json<UninstallTime> as restate_sdk::serde::Serialize>::serialize(&Json(
                value.clone(),
            ))
            .unwrap();
            assert_eq!(encoded, json.as_bytes());
            assert_eq!(decode(json.as_bytes()), value);
        }
        assert!(UninstallTime::Unchecked(u64::MAX).confirmed_at().is_err());
    }

    /// `onboard_webhook`'s answer: the former `()` answer (an empty body)
    /// reads as `None`, and `None` is still an empty body.
    #[test]
    fn webhook_outcomes_answer_as_the_former_unit_did() {
        use restate_sdk::serde::{Deserialize as _, Serialize as _};
        type Answer = Option<Json<WebhookOutcome>>;
        assert!(
            Answer::deserialize(&mut bytes::Bytes::new())
                .unwrap()
                .is_none()
        );
        assert!(Answer::None.serialize().unwrap().is_empty());
        let unconfirmed = Some(Json(WebhookOutcome::DeletionNotConfirmed))
            .serialize()
            .unwrap();
        assert_eq!(unconfirmed, &b"\"deletion_not_confirmed\""[..]);
        assert_eq!(
            Answer::deserialize(&mut unconfirmed.clone())
                .unwrap()
                .map(Json::into_inner),
            Some(WebhookOutcome::DeletionNotConfirmed)
        );
    }
}
