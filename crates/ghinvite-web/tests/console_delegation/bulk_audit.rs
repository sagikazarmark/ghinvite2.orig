//! Current GitHub-derived delegation reaches retained batches without a link.
use super::*;
use ghinvite_core::{
    bulk::{BulkBatchId, Entry, Manifest, Resolution, Review, RowEvidence},
    storage::{
        ConsoleStorage, RecordStorage,
        bulk::{BulkAudit, BulkStorage, Projection},
    },
    targets::{RepoRole, RepoTarget, Targets},
};

#[tokio::test]
async fn delegated_batch_audit_discovers_all_targets_before_showing_the_record() {
    for all_targets in [false, true] {
        let rpc = SettingsRpc::start(vec![]).await;
        let (app,cookie,storage)=fixture_with_reads(rpc.client.clone(),GithubUser::new("outsider",99),false,vec![
            Expectation::ok_json(Method::Get,"https://api.github.com/installation/repositories?per_page=100",json!({
                "total_count":2,"repositories":[
                    {"id":10,"full_name":"acme/api","owner":{"id":9001,"login":"acme"}},
                    {"id":11,"full_name":"acme/web","owner":{"id":9001,"login":"acme"}},
                ]
            })),
            sign_in::repo_identity(10,"acme/api",9001),sign_in::repo_identity(11,"acme/web",9001),
            sign_in::repo_admin("acme/api",99,"outsider"),
            Expectation::ok_json(Method::Get,"https://api.github.com/repos/acme/web/collaborators/outsider/permission",
                json!({"user":{"id":99,"login":"outsider"},"role_name":if all_targets {"admin"} else {"read"}})),
        ]).await;
        let at = chrono::Utc::now();
        storage
            .put_account_settings(&AccountSettings {
                account_id: 9001,
                revision: 1,
                repo_admin_delegation: true,
                changed_by: 42,
                changed_at: at,
            })
            .await
            .unwrap();
        storage
            .upsert_user(&ghinvite_core::User {
                user_id: 42,
                login: "octocat".into(),
                avatar_url: None,
                github_created_at: None,
                last_seen_at: at,
            })
            .await
            .unwrap();
        let batch = BulkBatchId::new();
        let review = Review::new(
            Manifest {
                batch_id: batch,
                admin: ghinvite_core::invitation_link::AccountAdmin {
                    account_id: 9001,
                    user_id: 42,
                },
                installation_id: sign_in::acme_installation().installation_id,
                organization_installation_id: None,
                access_lifetime_seconds: None,
                targets: Targets {
                    organization: None,
                    teams: vec![],
                    repos: vec![
                        RepoTarget {
                            repo_id: 10,
                            repo_full_name: "acme/api".into(),
                            grant: RepoRole::Write,
                        },
                        RepoTarget {
                            repo_id: 11,
                            repo_full_name: "acme/web".into(),
                            grant: RepoRole::Read,
                        },
                    ],
                },
                entries: vec![Entry::Handle {
                    login: "alice".into(),
                }],
            },
            vec![RowEvidence {
                row: 1,
                resolution: Resolution::Found {
                    user_id: 91,
                    login: "alice".into(),
                },
                previews: vec![],
                all_targets_satisfied: false,
                observed_at: at,
                next_budget_slot: None,
            }],
            at,
        )
        .unwrap();
        storage
            .project_bulk_projection(&Projection {
                review: review.clone(),
                events: vec![BulkAudit {
                    operation_id: RequestId::new().to_string().try_into().unwrap(),
                    actor: review.manifest.admin.clone(),
                    at,
                    revision: 1,
                    kind: ghinvite_core::audit::EventType::BulkBatchCreated,
                    removed_rows: Default::default(),
                }],
            })
            .await
            .unwrap();
        assert!(
            storage
                .list_invitation_links_for_account(9001)
                .await
                .unwrap()
                .is_empty()
        );
        let response = request(
            &app,
            &cookie,
            "GET",
            "/console/accounts/acme/audit?event=bulk_batch.created",
            "",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers()["cache-control"]
                .to_str()
                .unwrap()
                .contains("no-store")
        );
        let body = html(response).await;
        assert_eq!(
            body.contains(&batch.to_string()),
            all_targets,
            "partial repository authority cannot expose a batch audit record"
        );
        assert!(!body.contains("Repository-admin delegation"));
        assert!(!body.contains("More recent events"));
        assert!(!body.contains("Older events"));
        assert!(rpc.calls().is_empty());
    }
}
