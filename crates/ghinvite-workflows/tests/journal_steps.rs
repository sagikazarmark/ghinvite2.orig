//! Current production named-run paths, observed through real Restate SQL journals.
//! This is a deployment review aid, not a cross-release replay guarantee.
#![cfg(feature = "integration")]
use ghinvite_core::{GithubInvitationId, InvitationLinkId, RequestId, storage::RecordStorage};
use ghinvite_e2e::{Call, Harness, RunPath, journal::JournalAudit};
use serde_json::{Value, json};
use std::sync::Arc;
const SERVICES: &[&str] = &["InvitationLink", "RepositoryDelivery"];
// Source-derived expectations, never generated from observed journals.
const PATHS: &[RunPath] = &[
    RunPath::new("InvitationLink", "prepare_access_removal", &[]),
    RunPath::new("RepositoryDelivery", "prepare_reissue", &[]),
    RunPath::new("RepositoryDelivery", "cancel_unsent", &[]),
    RunPath::new("RepositoryDelivery", "control", &[]),
    RunPath::new("RepositoryDelivery", "control", &["delivery_control_time"]),
    RunPath::new(
        "RepositoryDelivery",
        "control",
        &[
            "control_attempt_fence",
            "delivery_control_time",
            "delivery_audit_time",
        ],
    ),
    RunPath::new("RepositoryDelivery", "control_wake", &[]),
    RunPath::new(
        "RepositoryDelivery",
        "control_wake",
        &[
            "control_attempt_fence",
            "delivery_control_gate_time",
            "delivery_time",
            "pre_read_repository",
            "guarded_github_create",
            "delivery_control_gate_time",
        ],
    ),
    RunPath::new(
        "RepositoryDelivery",
        "create",
        &[
            "control_attempt_fence",
            "delivery_control_gate_time",
            "delivery_audit_time",
        ],
    ),
    RunPath::new(
        "RepositoryDelivery",
        "create",
        &["delivery_control_gate_time"],
    ),
    RunPath::new(
        "RepositoryDelivery",
        "create",
        &[
            "control_attempt_fence",
            "delivery_control_gate_time",
            "delivery_time",
            "pre_read_repository",
            "delivery_audit_time",
            "control_attempt_fence",
            "delivery_control_gate_time",
        ],
    ),
    RunPath::new(
        "RepositoryDelivery",
        "reconcile",
        &["delivery_control_gate_time"],
    ),
    RunPath::new(
        "RepositoryDelivery",
        "create",
        &[
            "delivery_time",
            "pre_read_repository",
            "guarded_github_create",
            "outcome_unknown_since",
            "delivery_audit_time",
        ],
    ),
    RunPath::new(
        "RepositoryDelivery",
        "create",
        &[
            "release_rejected_generation",
            "delivery_time",
            "pre_read_repository",
            "guarded_github_create",
            "outcome_unknown_since",
            "delivery_audit_time",
        ],
    ),
    RunPath::new(
        "RepositoryDelivery",
        "recheck",
        &[
            "delivery_time",
            "pre_read_repository",
            "guarded_github_create",
            "outcome_unknown_since",
            "delivery_audit_time",
        ],
    ),
    RunPath::new(
        "RepositoryDelivery",
        "recheck",
        &[
            "release_rejected_generation",
            "delivery_time",
            "pre_read_repository",
            "guarded_github_create",
            "outcome_unknown_since",
            "delivery_audit_time",
        ],
    ),
    RunPath::new("InvitationLink", "create", &[]),
    RunPath::new(
        "InvitationLink",
        "create",
        &["creation_time", "decide_creation"],
    ),
    RunPath::new("InvitationLink", "link_status", &[]),
    RunPath::new("InvitationLink", "offer", &[]),
    RunPath::new("InvitationLink", "offer", &["offer_time"]),
    RunPath::new("InvitationLink", "pause", &[]),
    RunPath::new("InvitationLink", "pause", &["lifecycle_time"]),
    RunPath::new("InvitationLink", "resume", &[]),
    RunPath::new("InvitationLink", "resume", &["lifecycle_time"]),
    RunPath::new("InvitationLink", "rotate_secret", &[]),
    RunPath::new("InvitationLink", "rotate_secret", &["rotation_time"]),
    RunPath::new("InvitationLink", "admit", &["admission_gate_time"]),
    RunPath::new("InvitationLink", "revoke", &[]),
    RunPath::new("InvitationLink", "revoke_status", &[]),
    RunPath::new("InvitationLink", "cascade_progress", &[]),
    RunPath::new("InvitationLink", "revoke", &["revocation_time"]),
    RunPath::new("InvitationLink", "update_metadata", &[]),
    RunPath::new(
        "InvitationLink",
        "update_metadata",
        &["metadata_time", "metadata_event"],
    ),
    RunPath::new("InvitationLink", "prepare_attempt", &[]),
    RunPath::new("InvitationLink", "admit", &[]),
    RunPath::new(
        "InvitationLink",
        "admit",
        &["admission_gate_time", "admission_time", "decide_admission"],
    ),
    RunPath::new("InvitationLink", "requester_page", &[]),
    RunPath::new("InvitationLink", "requester_page", &["requester_page_time"]),
    RunPath::new("RepositoryDelivery", "on_webhook", &[]),
    RunPath::new("RepositoryDelivery", "reconcile", &[]),
    RunPath::new("RepositoryDelivery", "status", &[]),
    RunPath::new("RepositoryDelivery", "refresh", &[]),
    RunPath::new(
        "RepositoryDelivery",
        "refresh",
        &["repository_refresh_time"],
    ),
    RunPath::new("RepositoryDelivery", "cancel", &[]),
    RunPath::new("RepositoryDelivery", "drive_cancellation", &[]),
    RunPath::new("RepositoryDelivery", "tick_own_expiry", &[]),
    RunPath::new("RepositoryDelivery", "cascade_progress", &[]),
    RunPath::new("RepositoryDelivery", "tick_expire", &[]),
    RunPath::new(
        "RepositoryDelivery",
        "tick_expire",
        &["withdraw_known_invitation"],
    ),
    RunPath::new("RepositoryDelivery", "create", &[]),
    RunPath::new(
        "RepositoryDelivery",
        "create",
        &["recovery_time", "recover_unknown_invitation"],
    ),
    RunPath::new(
        "RepositoryDelivery",
        "create",
        &[
            "delivery_time",
            "pre_read_repository",
            "guarded_github_create",
        ],
    ),
    RunPath::new(
        "RepositoryDelivery",
        "create",
        &[
            "delivery_time",
            "pre_read_repository",
            "guarded_github_create",
            "release_refused_put",
        ],
    ),
    RunPath::new(
        "RepositoryDelivery",
        "create",
        &[
            "release_rejected_generation",
            "delivery_time",
            "pre_read_repository",
            "guarded_github_create",
        ],
    ),
    RunPath::new(
        "RepositoryDelivery",
        "create",
        &[
            "release_rejected_generation",
            "delivery_time",
            "pre_read_repository",
            "guarded_github_create",
            "release_refused_put",
        ],
    ),
    // First entry into a bounded blocked reason is audited; repeated policy
    // checks retain the paths above without creating a second audit episode.
    RunPath::new(
        "RepositoryDelivery",
        "create",
        &[
            "delivery_time",
            "pre_read_repository",
            "delivery_audit_time",
        ],
    ),
    RunPath::new(
        "RepositoryDelivery",
        "create",
        &[
            "delivery_time",
            "pre_read_repository",
            "guarded_github_create",
            "release_refused_put",
            "delivery_audit_time",
        ],
    ),
    RunPath::new("RepositoryDelivery", "recheck", &[]),
    RunPath::new(
        "RepositoryDelivery",
        "recheck",
        &["recovery_time", "recover_unknown_invitation"],
    ),
    RunPath::new(
        "RepositoryDelivery",
        "recheck",
        &["delivery_time", "pre_read_repository"],
    ),
    RunPath::new(
        "RepositoryDelivery",
        "recheck",
        &[
            "delivery_time",
            "pre_read_repository",
            "guarded_github_create",
        ],
    ),
    RunPath::new(
        "RepositoryDelivery",
        "recheck",
        &[
            "delivery_time",
            "pre_read_repository",
            "guarded_github_create",
            "release_refused_put",
        ],
    ),
    RunPath::new(
        "RepositoryDelivery",
        "recheck",
        &[
            "release_rejected_generation",
            "delivery_time",
            "pre_read_repository",
            "guarded_github_create",
        ],
    ),
    RunPath::new(
        "RepositoryDelivery",
        "recheck",
        &[
            "release_rejected_generation",
            "delivery_time",
            "pre_read_repository",
            "guarded_github_create",
            "release_refused_put",
        ],
    ),
];
struct Runtime {
    harness: Harness,
    audit: JournalAudit,
    storage: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    sql: sqlx::SqlitePool,
    database: std::path::PathBuf,
}
impl Runtime {
    async fn start() -> Self {
        let harness = Harness::start().await;
        let database =
            std::env::temp_dir().join(format!("platform-journal-{}.sqlite", RequestId::new()));
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::at_path(&database)
                .await
                .unwrap(),
        );
        let sql = sqlx::SqlitePool::connect_with(
            sqlx::sqlite::SqliteConnectOptions::new().filename(&database),
        )
        .await
        .unwrap();
        for (user_id, login) in [(7, "creator"), (8, "alice")] {
            storage
                .upsert_user(&ghinvite_core::User {
                    user_id,
                    login: login.into(),
                    avatar_url: None,
                    github_created_at: None,
                    last_seen_at: chrono::Utc::now(),
                })
                .await
                .unwrap();
        }
        let apps = Arc::new(
            ghinvite_workflows::RepositoryGithub::new(
                ghinvite_e2e::github_transport(harness.client.clone()),
                &harness.github_url,
                ghinvite_e2e::repository_apps(&harness.github_url),
            )
            .unwrap(),
        );
        let state = ghinvite_workflows::AppState::new(storage.clone()).with_github_apps(apps);
        let builder =
            ghinvite_workflows::admission::bind(restate_sdk::endpoint::Endpoint::builder());
        let builder = ghinvite_workflows::request_owner::bind(builder);
        let builder = ghinvite_workflows::projection::bind(builder, storage.clone());
        let endpoint = ghinvite_workflows::delivery::bind(builder, state.clone())
            .bind(ghinvite_workflows::reconcile::Reconcile {
                state: state.clone(),
            })
            .bind(ghinvite_workflows::installation::Installation {
                state: state.clone(),
            })
            .bind(ghinvite_workflows::availability::InstallationProjection {
                state: state.clone(),
            })
            .bind(ghinvite_workflows::availability::AccountInstallation { state })
            .build();
        harness.restate.deploy(endpoint).await;
        JournalAudit::retain(&harness, SERVICES).await;
        let onboard = harness.restate.invoke(&Call::object("AccountInstallation", "repo:100", "onboard"), Some(&ghinvite_e2e::setup_command(json!({"app":"repo","installation_id":1,"actor_user_id":7,"account_id":100,"account_login":"acme","account_type":"Organization","selected_repos":"all","installed_at":chrono::Utc::now()}))), None).await;
        assert_eq!(onboard.status, 200, "{onboard:?}");
        Self {
            harness,
            audit: JournalAudit::default(),
            storage,
            sql,
            database,
        }
    }
    async fn invoke(
        &mut self,
        service: &str,
        key: impl ToString,
        handler: &str,
        input: &Value,
        status: u16,
    ) -> Value {
        let reply = self
            .audit
            .invoke(&self.harness, service, &key.to_string(), handler, input)
            .await;
        assert_eq!(reply.status, status, "{service}/{handler}: {reply:?}");
        reply.body
    }
    async fn ok(
        &mut self,
        service: &str,
        key: impl ToString,
        handler: &str,
        input: &Value,
    ) -> Value {
        self.invoke(service, key, handler, input, 200).await
    }
    async fn refuse_once(&self) {
        self.harness
            .client
            .post(format!("{}/outcomes", self.harness.github_url))
            .json(&json!({"owner":"acme","repo":"api","user":"alice","outcome":"access_lost_once"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
    async fn hold_release(&self, hold: bool) {
        let statement = if hold {
            "CREATE TRIGGER hold_release BEFORE UPDATE ON delivery_attempts WHEN NEW.retryable = 1 BEGIN SELECT RAISE(ABORT, 'fixture release unavailable'); END"
        } else {
            "DROP TRIGGER hold_release"
        };
        sqlx::query(statement).execute(&self.sql).await.unwrap();
    }
    async fn command(&self) -> ghinvite_core::delivery::CreateCommand {
        self.harness
            .client
            .delete(format!("{}/reset", self.harness.github_url))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        // Approved private issuance boundary, through production projection writers.
        let link: ghinvite_core::InvitationLink = serde_json::from_value(json!({"secret":{"secret_generation":1,"sha256":"2a".repeat(32)},
            "id":InvitationLinkId::new(),"installation_id":1,"account_id":100,"created_by":7,
            "created_at":chrono::Utc::now(),"expires_at":null,"max_uses":null,"uses_count":1,
            "approval_required":true,"description":"Journal delivery",
            "internal_note":null,"revoked_at":null,"revoked_by":null,
            "targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]}
        }))
        .unwrap();
        let request: ghinvite_core::InvitationRequest = serde_json::from_value(json!({
            "id":RequestId::new(),"invitation_link_id":link.id,"requester_id":8,"justification":"Docs",
            "state":"approved","decided_by":7,"decided_at":chrono::Utc::now(),"decline_reason":null,
            "created_at":chrono::Utc::now(),"decision_deadline":null
        })).unwrap();
        ghinvite_core::storage::projection::fixture::seed(
            self.storage.as_ref(),
            &link,
            std::slice::from_ref(&request),
            1,
        )
        .await
        .unwrap();
        ghinvite_core::delivery::CreateCommand {
            batch: None,
            budget_attribution: Default::default(),
            access_lifetime_seconds: None,
            budget_share_pct: 50,
            delivery_generation: 1,
            invitation_id: Some(GithubInvitationId::new()),
            link_id: Some(link.id),
            request_id: request.id,
            approval_id: format!("fixture/{}", request.id),
            account_id: 100,
            installation_id: 1,
            requester_id: 8,
            repo_id: 10,
            repo_full_name: "acme/api".into(),
            permission: ghinvite_core::Permission::Push,
            approved_at: request.decided_at.unwrap(),
        }
    }
    async fn create_delivery(&mut self, command: &ghinvite_core::delivery::CreateCommand) -> Value {
        self.ok(
            "RepositoryDelivery",
            command.delivery_key(),
            "create",
            &json!(command),
        )
        .await
    }
}
#[tokio::test]
async fn production_handler_journals_walk_fixed_step_table() {
    let mut r = Runtime::start().await;
    let id = InvitationLinkId::new();
    let creation = json!({"max_uses":1000,"secret_sha256":"2a".repeat(32),"link_id":id,"account_id":100,"installation_id":1,
        "admin":{"account_id":100,"user_id":7},"description":"Journal link","approval_required":true,
        "targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]}});
    r.ok("InvitationLink", id, "create", &creation).await;
    r.ok("InvitationLink", id, "create", &creation).await;
    let admin = json!({"link_id":id,"admin":{"account_id":100,"user_id":7}});
    r.ok("InvitationLink", id, "link_status", &admin).await;
    let offer = json!({"link_id":id,"secret_sha256":"2a".repeat(32),"secret_generation":1});
    r.ok("InvitationLink", id, "offer", &offer).await;
    r.invoke(
        "InvitationLink",
        id,
        "offer",
        &json!({"link_id":id,"secret_sha256":"00".repeat(32),"secret_generation":1}),
        404,
    )
    .await;
    for action in ["pause", "resume"] {
        let input = json!({"link_id":id,"admin":{"account_id":100,"user_id":7},"operation_id":RequestId::new()});
        r.ok("InvitationLink", id, action, &input).await;
        r.ok("InvitationLink", id, action, &input).await;
    }

    let metadata = json!({"link_id":id,"admin":{"account_id":100,"user_id":7},"description":"Updated","internal_note":"Private"});
    r.ok("InvitationLink", id, "update_metadata", &metadata)
        .await;
    r.ok("InvitationLink", id, "update_metadata", &metadata)
        .await;
    let attempt = json!({"scope_evidence":[],"secret_sha256":"2a".repeat(32),"secret_generation":1,"link_id":id,"operation_id":RequestId::new(),"requester_id":8,"justification":"Docs"});
    r.ok("InvitationLink", id, "prepare_attempt", &attempt)
        .await;
    let page = json!({"secret_sha256":"2a".repeat(32),"secret_generation":1,"link_id":id,"requester_id":8});
    r.ok("InvitationLink", id, "requester_page", &page).await;
    r.invoke(
        "InvitationLink",
        id,
        "requester_page",
        &json!({"secret_sha256":"2a".repeat(32),"secret_generation":1,"link_id":id,"requester_id":8,"operation_id":RequestId::new()}),
        404,
    )
    .await;
    r.ok("InvitationLink", id, "admit", &attempt).await;
    r.ok("InvitationLink", id, "admit", &attempt).await;
    r.ok("InvitationLink", id, "requester_page", &page).await;
    let rotation = json!({"link_id":id,"operation_id":RequestId::new(),"admin":{"account_id":100,"user_id":7},"new_sha256":"2b".repeat(32)});
    r.ok("InvitationLink", id, "rotate_secret", &rotation).await;
    r.ok("InvitationLink", id, "rotate_secret", &rotation).await;
    let mut rejected = attempt.clone();
    rejected["operation_id"] = json!(RequestId::new());
    r.ok("InvitationLink", id, "admit", &rejected).await;
    let mut revocation = admin.clone();
    revocation["operation_id"] = json!(RequestId::new());
    revocation["cascade"] = json!({"decline_pending_requests":false,"cancel_pending_invitations":false,"remove_granted_access":false});
    r.ok("InvitationLink", id, "revoke", &revocation).await;
    r.ok("InvitationLink", id, "revoke", &revocation).await;
    r.ok("InvitationLink", id, "revoke_status", &revocation)
        .await;
    r.ok("InvitationLink", id, "cascade_progress", &admin).await;
    let command = r.command().await;
    assert_eq!(
        r.ok(
            "RepositoryDelivery",
            command.delivery_key(),
            "refresh",
            &Value::Null
        )
        .await["kind"],
        "not_tracked"
    );
    r.create_delivery(&command).await;
    r.create_delivery(&command).await;
    assert_eq!(
        r.ok(
            "RepositoryDelivery",
            command.delivery_key(),
            "refresh",
            &Value::Null
        )
        .await["kind"],
        "requested"
    );
    assert_eq!(
        r.ok(
            "RepositoryDelivery",
            command.delivery_key(),
            "refresh",
            &Value::Null
        )
        .await["kind"],
        "throttled"
    );
    r.ok(
        "RepositoryDelivery",
        command.delivery_key(),
        "recheck",
        &json!({"command":command,"generation":999}),
    )
    .await;
    let status = r
        .ok(
            "RepositoryDelivery",
            command.delivery_key(),
            "status",
            &Value::Null,
        )
        .await;
    let snapshot: ghinvite_core::delivery::DeliverySnapshot =
        serde_json::from_value(status).unwrap();
    r.ok(
        "RepositoryDelivery",
        command.delivery_key(),
        "reconcile",
        &json!(ghinvite_workflows::settlement_policy::Observation::new(
            &snapshot,
            chrono::Utc::now(),
            ghinvite_workflows::settlement_policy::Evidence::Absent {
                availability_verified: true,
                role_name: Some(ghinvite_core::targets::EffectiveRole::None),
                direct_collaborator: None,
                membership: None,
            }
        )),
    )
    .await;
    r.ok(
        "RepositoryDelivery",
        command.delivery_key(),
        "on_webhook",
        &json!({"invitation_id":command.invitation_id,"action":"declined","at":chrono::Utc::now()}),
    )
    .await;
    for handler in ["cancel", "tick_expire"] {
        let command = r.command().await;
        r.create_delivery(&command).await;
        let input = json!({"invitation_id":command.invitation_id,"installation_id":1,"by_user":7,"at":chrono::Utc::now()});
        r.invoke(
            "RepositoryDelivery",
            command.delivery_key(),
            handler,
            &input,
            if handler == "cancel" { 409 } else { 200 },
        )
        .await;
        // Owned legacy cancellation cannot authorize a DELETE, including when
        // addressed to a different installation. Expiry remains an observation.
        r.invoke("RepositoryDelivery",command.delivery_key(),handler,&json!({"invitation_id":command.invitation_id,"installation_id":999,"by_user":7,"at":chrono::Utc::now()}),if handler == "cancel" {409} else {200}).await;
        if handler == "cancel" {
            r.ok(
                "RepositoryDelivery",
                command.delivery_key(),
                "drive_cancellation",
                &Value::Null,
            )
            .await;
            r.ok(
                "RepositoryDelivery",
                command.delivery_key(),
                "tick_own_expiry",
                &json!({"generation":1,"upstream_id":99}),
            )
            .await;
            r.ok("RepositoryDelivery",command.delivery_key(),"cascade_progress",&json!({"identity":{"link_id":command.link_id,"request_id":command.request_id,"kind":"repository","target_id":command.repo_id,"delivery_generation":1},"required_control_revision":1})).await;
        }
    }
    // S1 can stop without entering S4; both handlers retain that named path.
    let command = r.command().await;
    r.harness
        .client
        .post(format!("{}/identity", r.harness.github_url))
        .json(&json!({"addressed_id":999}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let stopped = r.create_delivery(&command).await;
    assert_eq!(stopped["outcome"]["kind"], "blocked");
    r.ok(
        "RepositoryDelivery",
        command.delivery_key(),
        "recheck",
        &json!({"command":command,"generation":1}),
    )
    .await;
    for handler in ["create", "recheck"] {
        for hold in [false, true] {
            for refuse_again in [false, true] {
                let command = r.command().await;
                if hold {
                    r.hold_release(true).await;
                }
                r.refuse_once().await;
                let receipt = r.create_delivery(&command).await;
                assert_eq!(receipt["outcome"]["kind"], "blocked", "{receipt}");
                if hold {
                    r.hold_release(false).await;
                }
                if refuse_again {
                    r.refuse_once().await;
                }
                if handler == "create" {
                    r.create_delivery(&command).await;
                } else {
                    r.ok(
                        "RepositoryDelivery",
                        command.delivery_key(),
                        "recheck",
                        &json!({"command":command,"generation":1}),
                    )
                    .await;
                }
            }
        }
    }
    for handler in ["create", "recheck"] {
        let command = r.command().await;
        if handler == "recheck" {
            // A prior refused write retains a scheduled generation. Recovery of
            // the next ambiguous write must stay read-only when that wake fires.
            r.refuse_once().await;
            assert_eq!(
                r.create_delivery(&command).await["outcome"]["kind"],
                "blocked"
            );
        }
        r.harness.client.post(format!("{}/outcomes",r.harness.github_url))
            .json(&json!({"owner":"acme","repo":"api","user":"alice","outcome":"created_response_lost"}))
            .send().await.unwrap().error_for_status().unwrap();
        assert_eq!(
            r.create_delivery(&command).await["outcome"]["kind"],
            "outcome_unknown"
        );
        if handler == "create" {
            assert_eq!(
                r.create_delivery(&command).await["outcome"]["kind"],
                "created"
            );
        } else {
            r.ok(
                "RepositoryDelivery",
                command.delivery_key(),
                "recheck",
                &json!({"command":command,"generation":1}),
            )
            .await;
            let snapshot = r
                .ok(
                    "RepositoryDelivery",
                    command.delivery_key(),
                    "status",
                    &Value::Null,
                )
                .await;
            assert_eq!(snapshot["create"]["outcome"]["kind"], "created");
        }
    }
    // Newly uncertain outcomes date uncertainty once, including a retry whose
    // positively rejected prior generation still needs its release acknowledged.
    for handler in ["create", "recheck"] {
        for hold in [false, true] {
            let command = r.command().await;
            if hold || handler == "recheck" {
                if hold {
                    r.hold_release(true).await;
                }
                r.refuse_once().await;
                assert_eq!(
                    r.create_delivery(&command).await["outcome"]["kind"],
                    "blocked"
                );
                if hold {
                    r.hold_release(false).await;
                }
            }
            r.harness.client.post(format!("{}/outcomes", r.harness.github_url))
                .json(&json!({"owner":"acme","repo":"api","user":"alice","outcome":"created_response_lost"}))
                .send().await.unwrap().error_for_status().unwrap();
            if handler == "create" {
                assert_eq!(
                    r.create_delivery(&command).await["outcome"]["kind"],
                    "outcome_unknown"
                );
            } else {
                r.ok(
                    "RepositoryDelivery",
                    command.delivery_key(),
                    "recheck",
                    &json!({"command":command,"generation":1}),
                )
                .await;
                let snapshot = r
                    .ok(
                        "RepositoryDelivery",
                        command.delivery_key(),
                        "status",
                        &Value::Null,
                    )
                    .await;
                assert_eq!(snapshot["create"]["outcome"]["kind"], "outcome_unknown");
            }
        }
    }
    // Control paths come from the adapter's S0 ordering, not observed output.
    let command = r.command().await;
    let identity = json!({"link_id":command.link_id,"request_id":command.request_id,"kind":"repository","target_id":command.repo_id,"delivery_generation":command.delivery_generation});
    let window = json!({"epoch":1,"window_start":command.approved_at-chrono::Duration::minutes(1),"paused_at":chrono::Utc::now()});
    let make_control = |revision: u64, control: Value| json!({"envelope":{"identity":identity,"operation_id":RequestId::new(),"revision":revision,"hold_epoch":1,"at":chrono::Utc::now(),"control":control},"admitted_at":command.approved_at,"invitation_lifetime_seconds":604800});
    let hold = make_control(1, json!({"kind":"auto_pause","window":window}));
    r.ok(
        "RepositoryDelivery",
        command.delivery_key(),
        "control",
        &hold,
    )
    .await;
    r.ok(
        "RepositoryDelivery",
        command.delivery_key(),
        "control",
        &hold,
    )
    .await;
    assert_eq!(r.create_delivery(&command).await["outcome"]["kind"], "held");
    r.ok(
        "RepositoryDelivery",
        command.delivery_key(),
        "control",
        &make_control(2, json!({"kind":"active"})),
    )
    .await;
    let confirmed = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let snapshot = r
                .ok(
                    "RepositoryDelivery",
                    command.delivery_key(),
                    "status",
                    &Value::Null,
                )
                .await;
            if snapshot["create"]["outcome"]["kind"] == "created" {
                break snapshot;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    r.create_delivery(&command).await;
    let snapshot: ghinvite_core::delivery::DeliverySnapshot =
        serde_json::from_value(confirmed).unwrap();
    r.ok(
        "RepositoryDelivery",
        command.delivery_key(),
        "reconcile",
        &json!(ghinvite_workflows::settlement_policy::Observation::new(
            &snapshot,
            chrono::Utc::now(),
            ghinvite_workflows::settlement_policy::Evidence::Absent {
                availability_verified: true,
                role_name: Some(ghinvite_core::targets::EffectiveRole::None),
                direct_collaborator: None,
                membership: None
            }
        )),
    )
    .await;
    r.ok(
        "RepositoryDelivery",
        command.delivery_key(),
        "control_wake",
        &json!({"identity":identity,"generation":999}),
    )
    .await;

    // Release before create has no command to wake. A later blocked create binds
    // the command; the retained current generation can then be invoked exactly once.
    let command = r.command().await;
    let identity = json!({"link_id":command.link_id,"request_id":command.request_id,"kind":"repository","target_id":command.repo_id,"delivery_generation":command.delivery_generation});
    let window = json!({"epoch":1,"window_start":command.approved_at-chrono::Duration::minutes(1),"paused_at":chrono::Utc::now()});
    let make_control = |revision: u64, control: Value| json!({"envelope":{"identity":identity,"operation_id":RequestId::new(),"revision":revision,"hold_epoch":1,"at":chrono::Utc::now(),"control":control},"admitted_at":command.approved_at,"invitation_lifetime_seconds":604800});
    r.ok(
        "RepositoryDelivery",
        command.delivery_key(),
        "control",
        &make_control(1, json!({"kind":"auto_pause","window":window})),
    )
    .await;
    r.ok(
        "RepositoryDelivery",
        command.delivery_key(),
        "control",
        &make_control(2, json!({"kind":"active"})),
    )
    .await;
    r.harness
        .client
        .post(format!("{}/identity", r.harness.github_url))
        .json(&json!({"addressed_id":999}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        r.create_delivery(&command).await["outcome"]["kind"],
        "blocked"
    );
    r.harness
        .client
        .post(format!("{}/identity", r.harness.github_url))
        .json(&json!({"addressed_id":8}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    for _ in 0..2 {
        r.ok(
            "RepositoryDelivery",
            command.delivery_key(),
            "control_wake",
            &json!({"identity":identity,"generation":2}),
        )
        .await;
    }
    assert_eq!(
        r.ok(
            "RepositoryDelivery",
            command.delivery_key(),
            "prepare_reissue",
            &json!({"operation_id":RequestId::new(),"command":command})
        )
        .await,
        "pending"
    );
    r.invoke(
        "RepositoryDelivery", command.delivery_key(), "cancel_unsent",
        &json!({"operation_id":RequestId::new(),"admin":{"account_id":command.account_id,"user_id":7},"command":command}),
        503,
    ).await;
    r.invoke("InvitationLink",id,"prepare_access_removal",
        &json!({"link_id":id,"admin":{"account_id":100,"user_id":7},"operation_id":RequestId::new()}),503).await;
    let walked = r.audit.check(&r.harness, SERVICES, PATHS).await;
    assert_eq!(walked.handlers, 29);
    assert_eq!(walked.paths, PATHS.len());
    eprintln!("P1 journal table: {walked}");
    r.harness.finish().await;
    r.sql.close().await;
    drop(r.storage);
    std::fs::remove_file(r.database).unwrap();
}
