//! D1Storage: implements the full Storage trait using worker::D1Database.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ghinvite_core::audit::AuditEvent;
use ghinvite_core::storage::{GithubInvitationUpdate, RequestDecision, Result, Storage};
use ghinvite_core::{
    Account, GithubInvitation, GithubInvitationId, InvitationLink, InvitationLinkId,
    InvitationRequest, RequestId, SelectedRepos, User,
};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use wasm_bindgen::JsValue;
use worker::D1Database;

use crate::bind::{
    GithubInvitationRow, InstallationRow, InvitationLinkJoinRow, InvitationRequestRow, UserRow,
    classify_d1_error, collect_invitation_links, encode_selected_repos, try_one_invitation_link,
};

/// Wraps a `Future` and unsafely implements `Send`.
///
/// SAFETY: Cloudflare Workers (wasm32) are single-threaded: there is no
/// preemptive threading and `wasm32-unknown-unknown` has no `std::thread::spawn`.
/// No value ever crosses a thread boundary, so implementing `Send` here is sound.
struct WasmSend<F>(F);
unsafe impl<F> Send for WasmSend<F> {}
impl<F: Future> Future for WasmSend<F> {
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: we only project to the inner field, which was already pinned.
        unsafe { self.map_unchecked_mut(|s| &mut s.0).poll(cx) }
    }
}

/// Wrap an async block so that the resulting future is `Send`.
/// Used to satisfy the `async_trait` `+ Send` requirement on wasm32.
fn wasm_send<F: Future>(f: F) -> WasmSend<F> {
    WasmSend(f)
}

/// Map a `worker::Error` from `.bind()` to `ghinvite_core::storage::Error`.
fn bind_err(e: worker::Error) -> ghinvite_core::storage::Error {
    ghinvite_core::storage::Error::Database(e.to_string())
}

/// Extract the number of changed rows from a D1Result.
fn rows_changed(result: &worker::D1Result) -> ghinvite_core::storage::Result<usize> {
    result
        .meta()
        .map_err(|e| ghinvite_core::storage::Error::Database(e.to_string()))
        .map(|opt| opt.and_then(|m| m.changes).unwrap_or(0))
}

pub struct D1Storage {
    pub db: D1Database,
}

impl D1Storage {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

// SAFETY: D1Database holds a JS object reference (JsValue) which is !Send + !Sync upstream.
// This is sound only under the single-threaded Cloudflare Workers execution model where
// no OS threads exist and wasm32 has no thread-spawn capability. If SharedArrayBuffer-based
// threads are ever added to the build, these impls must be revisited.
unsafe impl Send for D1Storage {}
unsafe impl Sync for D1Storage {}

#[async_trait]
impl ghinvite_core::storage::projection::ProjectionStorage for D1Storage {
    async fn get_projected_request(
        &self,
        id: ghinvite_core::RequestId,
    ) -> Result<Option<ghinvite_core::storage::projection::RequestSnapshot>> {
        wasm_send(async {
            #[derive(serde::Deserialize)]
            struct Row { projection_content: String }
            let row = self.db.prepare("SELECT projection_content FROM invitation_requests WHERE id = ?1 AND projection_revision IS NOT NULL")
                .bind(&[JsValue::from_str(&id.to_string())]).map_err(bind_err)?
                .first::<Row>(None).await.map_err(classify_d1_error)?;
            row.map(|row| serde_json::from_str(&row.projection_content)
                .map_err(|_| ghinvite_core::storage::Error::Corrupt("projected request".into()))).transpose()
        }).await
    }

    async fn apply_transition(
        &self,
        envelope: &ghinvite_core::storage::projection::ProjectionEnvelope,
    ) -> Result<()> {
        use ghinvite_core::storage::projection::sql;
        let input = sql::encode(envelope)?;
        wasm_send(async {
            let statements = sql::STATEMENTS
                .iter()
                .map(|statement| {
                    self.db
                        .prepare(*statement)
                        .bind(&[JsValue::from_str(&input)])
                        .map_err(bind_err)
                })
                .collect::<Result<Vec<_>>>()?;
            self.db
                .batch(statements)
                .await
                .map_err(|error| sql::classify(error.to_string()))?;
            Ok(())
        })
        .await
    }
}

#[async_trait]
impl Storage for D1Storage {
    async fn list_github_invitations_for_request(
        &self,
        id: RequestId,
    ) -> Result<Vec<GithubInvitation>> {
        wasm_send(async {
            self.db
                .prepare(
                    "SELECT * FROM github_invitations WHERE invitation_request_id = ?1 ORDER BY id",
                )
                .bind(&[JsValue::from_str(&id.to_string())])
                .map_err(bind_err)?
                .all()
                .await
                .map_err(classify_d1_error)?
                .results::<GithubInvitationRow>()
                .map_err(bind_err)?
                .into_iter()
                .map(|row| row.try_into_domain())
                .collect()
        })
        .await
    }
    async fn delivery_attempt_exists(&self, id: GithubInvitationId) -> Result<bool> {
        wasm_send(async {
            #[derive(serde::Deserialize)]
            struct Row {
                invitation_id: String,
            }
            Ok(self
                .db
                .prepare("SELECT invitation_id FROM delivery_attempts WHERE invitation_id = ?1 AND retryable = 0")
                .bind(&[JsValue::from_str(&id.to_string())])
                .map_err(bind_err)?
                .first::<Row>(None)
                .await
                .map_err(classify_d1_error)?
                .is_some_and(|row| !row.invitation_id.is_empty()))
        })
        .await
    }
    async fn claim_delivery_attempt(
        &self,
        command: &ghinvite_core::delivery::CreateCommand,
    ) -> Result<Option<u64>> {
        wasm_send(async {
            let encoded = serde_json::to_string(command).map_err(|e| ghinvite_core::storage::Error::Corrupt(e.to_string()))?;
            #[derive(serde::Deserialize)]
            struct Generation { generation: u64 }
            let result = self.db.prepare("INSERT INTO delivery_attempts(invitation_id, command) VALUES (?1, ?2) ON CONFLICT(invitation_id) DO UPDATE SET generation = generation + 1, retryable = 0 WHERE retryable = 1 AND command = excluded.command RETURNING generation")
                .bind(&[JsValue::from_str(&command.invitation_id.to_string()), JsValue::from_str(&encoded)]).map_err(bind_err)?
                .first::<Generation>(None).await.map_err(classify_d1_error)?;
            #[derive(serde::Deserialize)]
            struct Row { command: String }
            let old = self.db.prepare("SELECT command FROM delivery_attempts WHERE invitation_id = ?1")
                .bind(&[JsValue::from_str(&command.invitation_id.to_string())]).map_err(bind_err)?
                .first::<Row>(None).await.map_err(classify_d1_error)?
                .ok_or(ghinvite_core::storage::Error::ProjectionDependency)?;
            if old.command != encoded { return Err(ghinvite_core::storage::Error::ProjectionInvariant("delivery attempt conflict".into())); }
            Ok(result.map(|r| r.generation))
        }).await
    }

    async fn reject_delivery_attempt(&self, id: GithubInvitationId, generation: u64) -> Result<()> {
        wasm_send(async {
            self.db.prepare("UPDATE delivery_attempts SET retryable = 1 WHERE invitation_id = ?1 AND generation = ?2")
                .bind(&[JsValue::from_str(&id.to_string()), JsValue::from_f64(generation as f64)]).map_err(bind_err)?
                .run().await.map_err(classify_d1_error)?;
            Ok(())
        }).await
    }

    async fn project_delivery(
        &self,
        receipt: &ghinvite_core::delivery::CreateReceipt,
    ) -> Result<()> {
        wasm_send(async {
            let encoded = serde_json::to_string(receipt).map_err(|e| ghinvite_core::storage::Error::Corrupt(e.to_string()))?;
            self.db.prepare("INSERT INTO delivery_outcomes(invitation_id, request_id, receipt) VALUES (?1, ?2, ?3) ON CONFLICT(invitation_id) DO UPDATE SET receipt = excluded.receipt WHERE json_extract(excluded.receipt, '$.revision') >= json_extract(delivery_outcomes.receipt, '$.revision')")
                .bind(&[JsValue::from_str(&receipt.command.invitation_id.to_string()), JsValue::from_str(&receipt.command.request_id.to_string()), JsValue::from_str(&encoded)]).map_err(bind_err)?
                .run().await.map_err(classify_d1_error)?;
            Ok(())
        }).await
    }

    async fn list_delivery_for_request(
        &self,
        id: RequestId,
    ) -> Result<Vec<ghinvite_core::delivery::CreateReceipt>> {
        wasm_send(async {
            #[derive(serde::Deserialize)]
            struct Row { receipt: String }
            self.db.prepare("SELECT receipt FROM delivery_outcomes WHERE request_id = ?1 ORDER BY invitation_id")
                .bind(&[JsValue::from_str(&id.to_string())]).map_err(bind_err)?
                .all().await.map_err(classify_d1_error)?.results::<Row>().map_err(bind_err)?
                .into_iter().map(|row| serde_json::from_str(&row.receipt).map_err(|e| ghinvite_core::storage::Error::Corrupt(e.to_string()))).collect()
        }).await
    }

    // -------- installations --------

    async fn insert_installation(&self, account: &Account) -> Result<()> {
        wasm_send(async {
            self.db
                .prepare(
                    "INSERT INTO installations
                       (installation_id, account_id, account_login, account_type,
                        installed_at, uninstalled_at, selected_repos)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )
                .bind(&[
                    JsValue::from_f64(account.installation_id as f64),
                    JsValue::from_f64(account.account_id as f64),
                    JsValue::from_str(&account.account_login),
                    JsValue::from_str(&account.account_type.to_string()),
                    JsValue::from_str(&account.installed_at.to_rfc3339()),
                    account
                        .uninstalled_at
                        .map(|d| JsValue::from_str(&d.to_rfc3339()))
                        .unwrap_or(JsValue::null()),
                    JsValue::from_str(&encode_selected_repos(&account.selected_repos)),
                ])
                .map_err(bind_err)?
                .run()
                .await
                .map_err(classify_d1_error)?;
            Ok(())
        })
        .await
    }

    async fn mark_installation_uninstalled(
        &self,
        installation_id: u64,
        when: DateTime<Utc>,
    ) -> Result<()> {
        wasm_send(async {
            let result = self
                .db
                .prepare(
                    "UPDATE installations SET uninstalled_at = ?1
                     WHERE installation_id = ?2 AND uninstalled_at IS NULL",
                )
                .bind(&[
                    JsValue::from_str(&when.to_rfc3339()),
                    JsValue::from_f64(installation_id as f64),
                ])
                .map_err(bind_err)?
                .run()
                .await
                .map_err(classify_d1_error)?;
            if rows_changed(&result)? == 0 {
                return Err(ghinvite_core::storage::Error::NotFound);
            }
            Ok(())
        })
        .await
    }

    async fn update_installation_repos(
        &self,
        installation_id: u64,
        selected: &SelectedRepos,
    ) -> Result<()> {
        let selected_str = encode_selected_repos(selected);
        wasm_send(async {
            let result = self
                .db
                .prepare(
                    "UPDATE installations SET selected_repos = ?1
                     WHERE installation_id = ?2 AND uninstalled_at IS NULL",
                )
                .bind(&[
                    JsValue::from_str(&selected_str),
                    JsValue::from_f64(installation_id as f64),
                ])
                .map_err(bind_err)?
                .run()
                .await
                .map_err(classify_d1_error)?;
            if rows_changed(&result)? == 0 {
                return Err(ghinvite_core::storage::Error::NotFound);
            }
            Ok(())
        })
        .await
    }

    async fn get_installation(&self, installation_id: u64) -> Result<Option<Account>> {
        wasm_send(async {
            let row: Option<InstallationRow> = self
                .db
                .prepare(
                    "SELECT installation_id, account_id, account_login, account_type,
                            installed_at, uninstalled_at, selected_repos
                     FROM installations WHERE installation_id = ?1",
                )
                .bind(&[JsValue::from_f64(installation_id as f64)])
                .map_err(bind_err)?
                .first::<InstallationRow>(None)
                .await
                .map_err(classify_d1_error)?;
            row.map(|r| r.try_into_domain()).transpose()
        })
        .await
    }

    async fn get_active_installation_by_account_id(
        &self,
        account_id: u64,
    ) -> Result<Option<Account>> {
        wasm_send(async {
            let row: Option<InstallationRow> = self
                .db
                .prepare(
                    "SELECT installation_id, account_id, account_login, account_type,
                            installed_at, uninstalled_at, selected_repos
                     FROM installations WHERE account_id = ?1 AND uninstalled_at IS NULL",
                )
                .bind(&[JsValue::from_f64(account_id as f64)])
                .map_err(bind_err)?
                .first::<InstallationRow>(None)
                .await
                .map_err(classify_d1_error)?;
            row.map(|r| r.try_into_domain()).transpose()
        })
        .await
    }

    async fn get_active_installation_by_login(&self, login: &str) -> Result<Option<Account>> {
        let login = login.to_string();
        wasm_send(async {
            let row: Option<InstallationRow> = self
                .db
                .prepare(
                    "SELECT installation_id, account_id, account_login, account_type,
                            installed_at, uninstalled_at, selected_repos
                     FROM installations WHERE account_login = ?1 AND uninstalled_at IS NULL",
                )
                .bind(&[JsValue::from_str(&login)])
                .map_err(bind_err)?
                .first::<InstallationRow>(None)
                .await
                .map_err(classify_d1_error)?;
            row.map(|r| r.try_into_domain()).transpose()
        })
        .await
    }

    async fn get_latest_installation_by_login(&self, login: &str) -> Result<Option<Account>> {
        let login = login.to_owned();
        wasm_send(async {
            let row: Option<InstallationRow> = self.db.prepare(
                "SELECT installation_id, account_id, account_login, account_type, installed_at, uninstalled_at, selected_repos FROM installations WHERE account_login = ? ORDER BY installed_at DESC, installation_id DESC LIMIT 1",
            ).bind(&[JsValue::from_str(&login)]).map_err(bind_err)?
                .first::<InstallationRow>(None).await.map_err(classify_d1_error)?;
            row.map(|r| r.try_into_domain()).transpose()
        }).await
    }

    async fn list_active_installations(&self) -> Result<Vec<Account>> {
        wasm_send(async {
            let rows: Vec<InstallationRow> = self
                .db
                .prepare(
                    "SELECT installation_id, account_id, account_login, account_type,
                            installed_at, uninstalled_at, selected_repos
                     FROM installations WHERE uninstalled_at IS NULL ORDER BY installed_at",
                )
                .all()
                .await
                .map_err(classify_d1_error)?
                .results::<InstallationRow>()
                .map_err(|e| ghinvite_core::storage::Error::Corrupt(e.to_string()))?;
            rows.into_iter().map(|r| r.try_into_domain()).collect()
        })
        .await
    }

    // -------- users --------

    async fn upsert_user(&self, user: &User) -> Result<()> {
        wasm_send(async {
            self.db
                .prepare(
                    "INSERT INTO users (user_id, login, avatar_url, last_seen_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(user_id) DO UPDATE SET
                         login = excluded.login,
                         avatar_url = excluded.avatar_url,
                         last_seen_at = excluded.last_seen_at",
                )
                .bind(&[
                    JsValue::from_f64(user.user_id as f64),
                    JsValue::from_str(&user.login),
                    user.avatar_url
                        .as_deref()
                        .map(JsValue::from_str)
                        .unwrap_or(JsValue::null()),
                    JsValue::from_str(&user.last_seen_at.to_rfc3339()),
                ])
                .map_err(bind_err)?
                .run()
                .await
                .map_err(classify_d1_error)?;
            Ok(())
        })
        .await
    }

    async fn get_user(&self, user_id: u64) -> Result<Option<User>> {
        wasm_send(async {
            let row: Option<UserRow> = self
                .db
                .prepare(
                    "SELECT user_id, login, avatar_url, last_seen_at
                     FROM users WHERE user_id = ?1",
                )
                .bind(&[JsValue::from_f64(user_id as f64)])
                .map_err(bind_err)?
                .first::<UserRow>(None)
                .await
                .map_err(classify_d1_error)?;
            row.map(|r| r.into_domain()).transpose()
        })
        .await
    }

    // -------- invitation links --------

    async fn insert_invitation_link(&self, link: &InvitationLink) -> Result<()> {
        // Pre-compute all values that need references to `link` before entering wasm_send.
        let id_str = link.id.to_string();
        let slug_str = link.slug.as_str().to_string();
        let installation_id = link.installation_id;
        let account_id = link.account_id;
        let created_by = link.created_by;
        let created_at = link.created_at.to_rfc3339();
        let expires_at = link
            .expires_at
            .map(|d| JsValue::from_str(&d.to_rfc3339()))
            .unwrap_or(JsValue::null());
        let max_uses = link
            .max_uses
            .map(|m| JsValue::from_f64(m as f64))
            .unwrap_or(JsValue::null());
        let uses_count = link.uses_count;
        let permission_str = link.permission.to_string();
        let approval_required = link.approval_required;
        let description = link.description.clone();
        let internal_note = link
            .internal_note
            .as_deref()
            .map(JsValue::from_str)
            .unwrap_or(JsValue::null());
        let revoked_at = link
            .revoked_at
            .map(|d| JsValue::from_str(&d.to_rfc3339()))
            .unwrap_or(JsValue::null());
        let revoked_by = link
            .revoked_by
            .map(|r| JsValue::from_f64(r as f64))
            .unwrap_or(JsValue::null());
        let repos: Vec<(String, u64, String)> = link
            .repos
            .iter()
            .map(|r| (id_str.clone(), r.repo_id, r.repo_full_name.clone()))
            .collect();

        wasm_send(async {
            let mut stmts = Vec::new();

            stmts.push(
                self.db
                    .prepare(
                        "INSERT INTO invitation_links
                           (id, slug, installation_id, account_id, created_by, created_at,
                            expires_at, max_uses, uses_count, permission, approval_required,
                            description, internal_note, revoked_at, revoked_by)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                    )
                    .bind(&[
                        JsValue::from_str(&id_str),
                        JsValue::from_str(&slug_str),
                        JsValue::from_f64(installation_id as f64),
                        JsValue::from_f64(account_id as f64),
                        JsValue::from_f64(created_by as f64),
                        JsValue::from_str(&created_at),
                        expires_at,
                        max_uses,
                        JsValue::from_f64(uses_count as f64),
                        JsValue::from_str(&permission_str),
                        JsValue::from_f64(if approval_required { 1.0 } else { 0.0 }),
                        JsValue::from_str(&description),
                        internal_note,
                        revoked_at,
                        revoked_by,
                    ])
                    .map_err(bind_err)?,
            );

            for (link_id, repo_id, repo_full_name) in &repos {
                stmts.push(
                    self.db
                        .prepare(
                            "INSERT INTO invitation_link_repos (invitation_link_id, repo_id, repo_full_name)
                             VALUES (?1, ?2, ?3)",
                        )
                        .bind(&[
                            JsValue::from_str(link_id),
                            JsValue::from_f64(*repo_id as f64),
                            JsValue::from_str(repo_full_name),
                        ])
                        .map_err(bind_err)?,
                );
            }

            self.db.batch(stmts).await.map_err(classify_d1_error)?;
            Ok(())
        })
        .await
    }

    async fn update_invitation_link_metadata(
        &self,
        account_id: u64,
        id: InvitationLinkId,
        description: &str,
        internal_note: Option<&str>,
    ) -> Result<()> {
        let id_str = id.to_string();
        wasm_send(async {
            let result = self
                .db
                .prepare(
                    "UPDATE invitation_links SET description = ?1, internal_note = ?2
                     WHERE account_id = ?3 AND id = ?4",
                )
                .bind(&[
                    JsValue::from_str(description),
                    internal_note
                        .map(JsValue::from_str)
                        .unwrap_or(JsValue::NULL),
                    JsValue::from_f64(account_id as f64),
                    JsValue::from_str(&id_str),
                ])
                .map_err(bind_err)?
                .run()
                .await
                .map_err(classify_d1_error)?;
            if rows_changed(&result)? == 0 {
                return Err(ghinvite_core::storage::Error::NotFound);
            }
            Ok(())
        })
        .await
    }

    async fn mark_invitation_link_revoked(
        &self,
        id: InvitationLinkId,
        by_user: u64,
        when: DateTime<Utc>,
    ) -> Result<()> {
        let id_str = id.to_string();
        let when_str = when.to_rfc3339();
        wasm_send(async {
            let result = self
                .db
                .prepare(
                    "UPDATE invitation_links SET revoked_at = ?1, revoked_by = ?2
                     WHERE id = ?3 AND revoked_at IS NULL",
                )
                .bind(&[
                    JsValue::from_str(&when_str),
                    JsValue::from_f64(by_user as f64),
                    JsValue::from_str(&id_str),
                ])
                .map_err(bind_err)?
                .run()
                .await
                .map_err(classify_d1_error)?;
            if rows_changed(&result)? == 0 {
                return Err(ghinvite_core::storage::Error::NotFound);
            }
            Ok(())
        })
        .await
    }

    async fn get_invitation_link_by_id(
        &self,
        id: InvitationLinkId,
    ) -> Result<Option<InvitationLink>> {
        let id_str = id.to_string();
        wasm_send(async {
            let rows: Vec<InvitationLinkJoinRow> = self
                .db
                .prepare(
                    "SELECT l.id, l.slug, l.installation_id, l.account_id, l.created_by,
                            l.created_at, l.expires_at, l.max_uses, l.uses_count, l.permission,
                            l.approval_required, l.description, l.internal_note, l.revoked_at, l.revoked_by,
                            r.repo_id, r.repo_full_name
                     FROM invitation_links l
                     LEFT JOIN invitation_link_repos r ON r.invitation_link_id = l.id
                     WHERE l.id = ?1
                     ORDER BY r.repo_id",
                )
                .bind(&[JsValue::from_str(&id_str)])
                .map_err(bind_err)?
                .all()
                .await
                .map_err(classify_d1_error)?
                .results::<InvitationLinkJoinRow>()
                .map_err(|e| ghinvite_core::storage::Error::Corrupt(e.to_string()))?;
            try_one_invitation_link(rows)
        })
        .await
    }

    async fn get_invitation_link_by_slug(&self, slug: &str) -> Result<Option<InvitationLink>> {
        let slug = slug.to_string();
        wasm_send(async {
            let rows: Vec<InvitationLinkJoinRow> = self
                .db
                .prepare(
                    "SELECT l.id, l.slug, l.installation_id, l.account_id, l.created_by,
                            l.created_at, l.expires_at, l.max_uses, l.uses_count, l.permission,
                            l.approval_required, l.description, l.internal_note, l.revoked_at, l.revoked_by,
                            r.repo_id, r.repo_full_name
                     FROM invitation_links l
                     LEFT JOIN invitation_link_repos r ON r.invitation_link_id = l.id
                     WHERE l.slug = ?1
                     ORDER BY r.repo_id",
                )
                .bind(&[JsValue::from_str(&slug)])
                .map_err(bind_err)?
                .all()
                .await
                .map_err(classify_d1_error)?
                .results::<InvitationLinkJoinRow>()
                .map_err(|e| ghinvite_core::storage::Error::Corrupt(e.to_string()))?;
            try_one_invitation_link(rows)
        })
        .await
    }

    async fn list_invitation_links_for_account(
        &self,
        account_id: u64,
    ) -> Result<Vec<InvitationLink>> {
        wasm_send(async {
            let rows: Vec<InvitationLinkJoinRow> = self
                .db
                .prepare(
                    "SELECT l.id, l.slug, l.installation_id, l.account_id, l.created_by,
                            l.created_at, l.expires_at, l.max_uses, l.uses_count, l.permission,
                            l.approval_required, l.description, l.internal_note, l.revoked_at, l.revoked_by,
                            r.repo_id, r.repo_full_name
                     FROM invitation_links l
                     LEFT JOIN invitation_link_repos r ON r.invitation_link_id = l.id
                     WHERE l.account_id = ?1
                     ORDER BY l.created_at DESC, l.id, r.repo_id",
                )
                .bind(&[JsValue::from_f64(account_id as f64)])
                .map_err(bind_err)?
                .all()
                .await
                .map_err(classify_d1_error)?
                .results::<InvitationLinkJoinRow>()
                .map_err(|e| ghinvite_core::storage::Error::Corrupt(e.to_string()))?;
            collect_invitation_links(rows)
        })
        .await
    }

    // -------- invitation requests --------

    async fn insert_invitation_request_and_increment_uses(
        &self,
        request: &InvitationRequest,
    ) -> Result<()> {
        let id_str = request.id.to_string();
        let invitation_link_id_str = request.invitation_link_id.to_string();
        let requester_id = request.requester_id;
        let justification = request
            .justification
            .as_deref()
            .map(JsValue::from_str)
            .unwrap_or(JsValue::null());
        let state_str = request.state.to_string();
        let decided_by = request
            .decided_by
            .map(|d| JsValue::from_f64(d as f64))
            .unwrap_or(JsValue::null());
        let decided_at = request
            .decided_at
            .map(|d| JsValue::from_str(&d.to_rfc3339()))
            .unwrap_or(JsValue::null());
        let decline_reason = request
            .decline_reason
            .as_deref()
            .map(JsValue::from_str)
            .unwrap_or(JsValue::null());
        let created_at_str = request.created_at.to_rfc3339();

        wasm_send(async {
            let stmt1 = self
                .db
                .prepare(
                    "INSERT INTO invitation_requests
                       (id, invitation_link_id, requester_id, justification, state,
                        decided_by, decided_at, decline_reason, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                )
                .bind(&[
                    JsValue::from_str(&id_str),
                    JsValue::from_str(&invitation_link_id_str),
                    JsValue::from_f64(requester_id as f64),
                    justification,
                    JsValue::from_str(&state_str),
                    decided_by,
                    decided_at,
                    decline_reason,
                    JsValue::from_str(&created_at_str),
                ])
                .map_err(bind_err)?;

            let stmt2 = self
                .db
                .prepare("UPDATE invitation_links SET uses_count = uses_count + 1 WHERE id = ?1")
                .bind(&[JsValue::from_str(&invitation_link_id_str)])
                .map_err(bind_err)?;

            let results = self
                .db
                .batch(vec![stmt1, stmt2])
                .await
                .map_err(classify_d1_error)?;
            // results[1] is the UPDATE on invitation_links; 0 rows_changed means link not found.
            if let Some(update_result) = results.get(1) {
                if rows_changed(update_result)? == 0 {
                    return Err(ghinvite_core::storage::Error::NotFound);
                }
            }
            Ok(())
        })
        .await
    }

    async fn record_request_decision(&self, decision: &RequestDecision) -> Result<()> {
        let state_str = decision.state.to_string();
        let decided_by = decision
            .decided_by
            .map(|d| JsValue::from_f64(d as f64))
            .unwrap_or(JsValue::null());
        let decided_at_str = decision.decided_at.to_rfc3339();
        let decline_reason = decision
            .decline_reason
            .as_deref()
            .map(JsValue::from_str)
            .unwrap_or(JsValue::null());
        let request_id_str = decision.request_id.to_string();

        wasm_send(async {
            let result = self
                .db
                .prepare(
                    "UPDATE invitation_requests
                     SET state = ?1, decided_by = ?2, decided_at = ?3, decline_reason = ?4
                     WHERE id = ?5 AND state = 'pending'",
                )
                .bind(&[
                    JsValue::from_str(&state_str),
                    decided_by,
                    JsValue::from_str(&decided_at_str),
                    decline_reason,
                    JsValue::from_str(&request_id_str),
                ])
                .map_err(bind_err)?
                .run()
                .await
                .map_err(classify_d1_error)?;
            if rows_changed(&result)? == 0 {
                return Err(ghinvite_core::storage::Error::NotFound);
            }
            Ok(())
        })
        .await
    }

    async fn get_invitation_request(&self, id: RequestId) -> Result<Option<InvitationRequest>> {
        let id_str = id.to_string();
        wasm_send(async {
            let row: Option<InvitationRequestRow> = self
                .db
                .prepare(
                    "SELECT id, invitation_link_id, requester_id, justification, state,
                            decided_by, decided_at, decline_reason, created_at
                     FROM invitation_requests WHERE id = ?1",
                )
                .bind(&[JsValue::from_str(&id_str)])
                .map_err(bind_err)?
                .first::<InvitationRequestRow>(None)
                .await
                .map_err(classify_d1_error)?;
            row.map(|r| r.try_into_domain()).transpose()
        })
        .await
    }

    async fn list_pending_requests_for_account(
        &self,
        account_id: u64,
    ) -> Result<Vec<InvitationRequest>> {
        wasm_send(async {
            let rows: Vec<InvitationRequestRow> = self
                .db
                .prepare(
                    "SELECT r.id, r.invitation_link_id, r.requester_id, r.justification, r.state,
                            r.decided_by, r.decided_at, r.decline_reason, r.created_at
                     FROM invitation_requests r
                     JOIN invitation_links l ON l.id = r.invitation_link_id
                     WHERE l.account_id = ?1 AND r.state = 'pending'
                     ORDER BY r.created_at",
                )
                .bind(&[JsValue::from_f64(account_id as f64)])
                .map_err(bind_err)?
                .all()
                .await
                .map_err(classify_d1_error)?
                .results::<InvitationRequestRow>()
                .map_err(|e| ghinvite_core::storage::Error::Corrupt(e.to_string()))?;
            rows.into_iter().map(|r| r.try_into_domain()).collect()
        })
        .await
    }

    async fn list_requests_for_link(
        &self,
        link_id: InvitationLinkId,
    ) -> Result<Vec<InvitationRequest>> {
        let link_id_str = link_id.to_string();
        wasm_send(async {
            let rows: Vec<InvitationRequestRow> = self
                .db
                .prepare(
                    "SELECT id, invitation_link_id, requester_id, justification, state,
                            decided_by, decided_at, decline_reason, created_at
                     FROM invitation_requests WHERE invitation_link_id = ?1
                     ORDER BY created_at DESC",
                )
                .bind(&[JsValue::from_str(&link_id_str)])
                .map_err(bind_err)?
                .all()
                .await
                .map_err(classify_d1_error)?
                .results::<InvitationRequestRow>()
                .map_err(|e| ghinvite_core::storage::Error::Corrupt(e.to_string()))?;
            rows.into_iter().map(|r| r.try_into_domain()).collect()
        })
        .await
    }

    // -------- github invitations --------

    async fn insert_github_invitation(&self, invitation: &GithubInvitation) -> Result<()> {
        let id_str = invitation.id.to_string();
        let request_id_str = invitation.invitation_request_id.to_string();
        let repo_id = invitation.repo_id;
        let github_invitation_id = invitation
            .github_invitation_id
            .map(|g| JsValue::from_f64(g as f64))
            .unwrap_or(JsValue::null());
        let state_str = invitation.state.to_string();
        let error_message = invitation
            .error_message
            .as_deref()
            .map(JsValue::from_str)
            .unwrap_or(JsValue::null());
        let created_at_str = invitation.created_at.to_rfc3339();
        let updated_at_str = invitation.updated_at.to_rfc3339();

        wasm_send(async {
            self.db
                .prepare(
                    "INSERT INTO github_invitations
                       (id, invitation_request_id, repo_id, github_invitation_id,
                        state, error_message, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )
                .bind(&[
                    JsValue::from_str(&id_str),
                    JsValue::from_str(&request_id_str),
                    JsValue::from_f64(repo_id as f64),
                    github_invitation_id,
                    JsValue::from_str(&state_str),
                    error_message,
                    JsValue::from_str(&created_at_str),
                    JsValue::from_str(&updated_at_str),
                ])
                .map_err(bind_err)?
                .run()
                .await
                .map_err(classify_d1_error)?;
            Ok(())
        })
        .await
    }

    async fn update_github_invitation(&self, update: &GithubInvitationUpdate) -> Result<()> {
        let state_str = update.state.to_string();
        let github_invitation_id = update
            .github_invitation_id
            .map(|g| JsValue::from_f64(g as f64))
            .unwrap_or(JsValue::null());
        let error_message = update
            .error_message
            .as_deref()
            .map(JsValue::from_str)
            .unwrap_or(JsValue::null());
        let updated_at_str = update.updated_at.to_rfc3339();
        let id_str = update.id.to_string();

        wasm_send(async {
            let result = self
                .db
                .prepare(
                    "UPDATE github_invitations
                     SET state = ?1,
                         github_invitation_id = ?2,
                         error_message = ?3,
                         updated_at = ?4
                     WHERE id = ?5",
                )
                .bind(&[
                    JsValue::from_str(&state_str),
                    github_invitation_id,
                    error_message,
                    JsValue::from_str(&updated_at_str),
                    JsValue::from_str(&id_str),
                ])
                .map_err(bind_err)?
                .run()
                .await
                .map_err(classify_d1_error)?;
            if rows_changed(&result)? == 0 {
                return Err(ghinvite_core::storage::Error::NotFound);
            }
            Ok(())
        })
        .await
    }

    async fn get_github_invitation(
        &self,
        id: GithubInvitationId,
    ) -> Result<Option<GithubInvitation>> {
        let id_str = id.to_string();
        wasm_send(async {
            let row: Option<GithubInvitationRow> = self
                .db
                .prepare(
                    "SELECT id, invitation_request_id, repo_id, github_invitation_id,
                            state, error_message, created_at, updated_at
                     FROM github_invitations WHERE id = ?1",
                )
                .bind(&[JsValue::from_str(&id_str)])
                .map_err(bind_err)?
                .first::<GithubInvitationRow>(None)
                .await
                .map_err(classify_d1_error)?;
            row.map(|r| r.try_into_domain()).transpose()
        })
        .await
    }

    async fn get_github_invitation_by_github_id(
        &self,
        github_id: u64,
    ) -> Result<Option<GithubInvitation>> {
        wasm_send(async {
            let row: Option<GithubInvitationRow> = self
                .db
                .prepare(
                    "SELECT id, invitation_request_id, repo_id, github_invitation_id,
                            state, error_message, created_at, updated_at
                     FROM github_invitations WHERE github_invitation_id = ?1",
                )
                .bind(&[JsValue::from_f64(github_id as f64)])
                .map_err(bind_err)?
                .first::<GithubInvitationRow>(None)
                .await
                .map_err(classify_d1_error)?;
            row.map(|r| r.try_into_domain()).transpose()
        })
        .await
    }

    async fn list_pending_github_invitations_for_installation(
        &self,
        installation_id: u64,
    ) -> Result<Vec<GithubInvitation>> {
        wasm_send(async {
            let rows: Vec<GithubInvitationRow> = self
                .db
                .prepare(
                    "SELECT g.id, g.invitation_request_id, g.repo_id, g.github_invitation_id,
                            g.state, g.error_message, g.created_at, g.updated_at
                     FROM github_invitations g
                     JOIN invitation_requests r ON r.id = g.invitation_request_id
                     JOIN invitation_links l ON l.id = r.invitation_link_id
                     WHERE l.installation_id = ?1
                       AND g.state IN ('sending', 'sent')
                     ORDER BY g.created_at",
                )
                .bind(&[JsValue::from_f64(installation_id as f64)])
                .map_err(bind_err)?
                .all()
                .await
                .map_err(classify_d1_error)?
                .results::<GithubInvitationRow>()
                .map_err(|e| ghinvite_core::storage::Error::Corrupt(e.to_string()))?;
            rows.into_iter().map(|r| r.try_into_domain()).collect()
        })
        .await
    }

    // -------- audit --------

    async fn invitation_link_belongs_to_account(
        &self,
        account_id: u64,
        id: InvitationLinkId,
    ) -> Result<bool> {
        wasm_send(async {
            Ok(self.db.prepare("SELECT 1 AS present FROM invitation_links WHERE id = ?1 AND account_id = ?2 LIMIT 1")
                .bind(&[JsValue::from_str(&id.to_string()), JsValue::from_f64(account_id as f64)]).map_err(bind_err)?
                .first::<serde_json::Value>(None).await.map_err(classify_d1_error)?.is_some())
        }).await
    }

    async fn list_audit_events(
        &self,
        account_id: u64,
        event: Option<ghinvite_core::audit::EventType>,
        position: ghinvite_core::storage::AuditPosition,
    ) -> Result<ghinvite_core::storage::AuditPage> {
        use ghinvite_core::storage::{AuditBoundary, AuditPage, AuditPosition, audit_read};
        wasm_send(async {
            let bindings = |position: AuditPosition| {
                let boundary = position.boundary();
                [
                    JsValue::from_f64(account_id as f64),
                    event
                        .map(|e| JsValue::from_str(e.as_str()))
                        .unwrap_or(JsValue::NULL),
                    boundary
                        .map(|b| JsValue::from_str(&audit_read::boundary_time(b)))
                        .unwrap_or(JsValue::NULL),
                    boundary
                        .map(|b| JsValue::from_str(&b.id.to_string()))
                        .unwrap_or(JsValue::NULL),
                ]
            };
            let rows = self
                .db
                .prepare(&audit_read::query(event, position, false))
                .bind(&bindings(position))
                .map_err(bind_err)?
                .all()
                .await
                .map_err(classify_d1_error)?
                .results::<crate::bind::AuditEventRow>()
                .map_err(classify_d1_error)?;
            let mut events = rows
                .into_iter()
                .map(|r| r.try_into_domain())
                .collect::<Result<Vec<_>>>()?;
            if matches!(position, AuditPosition::After(_)) {
                events.reverse();
            }
            let mut page = AuditPage {
                events,
                has_older: false,
                has_newer: false,
            };
            if let (Some(first), Some(last)) = (page.events.first(), page.events.last()) {
                for (seek, flag) in [
                    (
                        AuditPosition::After(AuditBoundary::from(first)),
                        &mut page.has_newer,
                    ),
                    (
                        AuditPosition::Before(AuditBoundary::from(last)),
                        &mut page.has_older,
                    ),
                ] {
                    *flag = self
                        .db
                        .prepare(&audit_read::query(event, seek, true))
                        .bind(&bindings(seek))
                        .map_err(bind_err)?
                        .first::<serde_json::Value>(None)
                        .await
                        .map_err(classify_d1_error)?
                        .is_some();
                }
            }
            Ok(page)
        })
        .await
    }

    async fn audit(&self, event: &AuditEvent) -> Result<()> {
        let id_str = event.id.to_string();
        let account_id = event.account_id;
        let occurred_at_str = event.occurred_at.to_rfc3339();
        let event_type_str = event.event_type.as_str();
        let actor_kind_str = event.actor_kind.to_string();
        let actor_id = event
            .actor_id
            .map(|a| JsValue::from_f64(a as f64))
            .unwrap_or(JsValue::null());
        let target_kind_str = event.target_kind.to_string();
        let target_id = event.target_id.clone();
        let metadata = if event.metadata.is_null() {
            JsValue::null()
        } else {
            JsValue::from_str(
                &serde_json::to_string(&event.metadata).expect("audit metadata serializes"),
            )
        };
        let request_id = event
            .request_id
            .as_deref()
            .map(JsValue::from_str)
            .unwrap_or(JsValue::null());

        let mut sql = String::from(
            "INSERT INTO audit_events
               (id, account_id, occurred_at, event_type, actor_kind, actor_id,
                target_kind, target_id, metadata, request_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        );
        // Only metadata commands supply a journaled, replay-stable event ID.
        if event.event_type == ghinvite_core::audit::EventType::InvitationLinkMetadataUpdated {
            sql.push_str(" ON CONFLICT(id) DO NOTHING");
        } else if matches!(
            event.event_type,
            ghinvite_core::audit::EventType::InstallationCreated
                | ghinvite_core::audit::EventType::InstallationReposChanged
                | ghinvite_core::audit::EventType::InstallationUninstalled
        ) {
            sql.push_str(ghinvite_core::storage::INSTALLATION_AUDIT_REPLAY);
        }

        wasm_send(async {
            self.db
                .prepare(&sql)
                .bind(&[
                    JsValue::from_str(&id_str),
                    JsValue::from_f64(account_id as f64),
                    JsValue::from_str(&occurred_at_str),
                    JsValue::from_str(event_type_str),
                    JsValue::from_str(&actor_kind_str),
                    actor_id,
                    JsValue::from_str(&target_kind_str),
                    JsValue::from_str(&target_id),
                    metadata,
                    request_id,
                ])
                .map_err(bind_err)?
                .run()
                .await
                .map_err(classify_d1_error)?;
            Ok(())
        })
        .await
    }
}
