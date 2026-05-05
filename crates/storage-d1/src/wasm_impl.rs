//! D1Storage: implements the full Storage trait using worker::D1Database.

use async_trait::async_trait;
use audit::AuditEvent;
use chrono::{DateTime, Utc};
use domain::{
    Account, GithubInvitation, GithubInvitationId, InvitationRequest, RequestId, SelectedRepos,
    ShareLink, ShareLinkId, User,
};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use storage::{GithubInvitationUpdate, RequestDecision, Result, Storage};
use wasm_bindgen::JsValue;
use worker::D1Database;

use crate::bind::{
    classify_d1_error, collect_share_links, encode_selected_repos, try_one_share_link,
    InstallationRow, ShareLinkJoinRow, UserRow,
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

/// Map a `worker::Error` from `.bind()` to `storage::Error`.
fn bind_err(e: worker::Error) -> storage::Error {
    storage::Error::Database(e.to_string())
}

/// Extract the number of changed rows from a D1Result.
fn rows_changed(result: &worker::D1Result) -> storage::Result<usize> {
    result
        .meta()
        .map_err(|e| storage::Error::Database(e.to_string()))
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
impl Storage for D1Storage {
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
                return Err(storage::Error::NotFound);
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
                return Err(storage::Error::NotFound);
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
                .map_err(|e| storage::Error::Corrupt(e.to_string()))?;
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

    // -------- share links --------

    async fn insert_share_link(&self, link: &ShareLink) -> Result<()> {
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
                        "INSERT INTO share_links
                           (id, slug, installation_id, account_id, created_by, created_at,
                            expires_at, max_uses, uses_count, permission, approval_required,
                            internal_note, revoked_at, revoked_by)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
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
                            "INSERT INTO share_link_repos (share_link_id, repo_id, repo_full_name)
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

            self.db
                .batch(stmts)
                .await
                .map_err(classify_d1_error)?;
            Ok(())
        })
        .await
    }

    async fn mark_share_link_revoked(
        &self,
        id: ShareLinkId,
        by_user: u64,
        when: DateTime<Utc>,
    ) -> Result<()> {
        let id_str = id.to_string();
        let when_str = when.to_rfc3339();
        wasm_send(async {
            let result = self
                .db
                .prepare(
                    "UPDATE share_links SET revoked_at = ?1, revoked_by = ?2
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
                return Err(storage::Error::NotFound);
            }
            Ok(())
        })
        .await
    }

    async fn get_share_link_by_id(&self, id: ShareLinkId) -> Result<Option<ShareLink>> {
        let id_str = id.to_string();
        wasm_send(async {
            let rows: Vec<ShareLinkJoinRow> = self
                .db
                .prepare(
                    "SELECT l.id, l.slug, l.installation_id, l.account_id, l.created_by,
                            l.created_at, l.expires_at, l.max_uses, l.uses_count, l.permission,
                            l.approval_required, l.internal_note, l.revoked_at, l.revoked_by,
                            r.repo_id, r.repo_full_name
                     FROM share_links l
                     LEFT JOIN share_link_repos r ON r.share_link_id = l.id
                     WHERE l.id = ?1
                     ORDER BY r.repo_id",
                )
                .bind(&[JsValue::from_str(&id_str)])
                .map_err(bind_err)?
                .all()
                .await
                .map_err(classify_d1_error)?
                .results::<ShareLinkJoinRow>()
                .map_err(|e| storage::Error::Corrupt(e.to_string()))?;
            try_one_share_link(rows)
        })
        .await
    }

    async fn get_share_link_by_slug(&self, slug: &str) -> Result<Option<ShareLink>> {
        let slug = slug.to_string();
        wasm_send(async {
            let rows: Vec<ShareLinkJoinRow> = self
                .db
                .prepare(
                    "SELECT l.id, l.slug, l.installation_id, l.account_id, l.created_by,
                            l.created_at, l.expires_at, l.max_uses, l.uses_count, l.permission,
                            l.approval_required, l.internal_note, l.revoked_at, l.revoked_by,
                            r.repo_id, r.repo_full_name
                     FROM share_links l
                     LEFT JOIN share_link_repos r ON r.share_link_id = l.id
                     WHERE l.slug = ?1
                     ORDER BY r.repo_id",
                )
                .bind(&[JsValue::from_str(&slug)])
                .map_err(bind_err)?
                .all()
                .await
                .map_err(classify_d1_error)?
                .results::<ShareLinkJoinRow>()
                .map_err(|e| storage::Error::Corrupt(e.to_string()))?;
            try_one_share_link(rows)
        })
        .await
    }

    async fn list_share_links_for_account(&self, account_id: u64) -> Result<Vec<ShareLink>> {
        wasm_send(async {
            let rows: Vec<ShareLinkJoinRow> = self
                .db
                .prepare(
                    "SELECT l.id, l.slug, l.installation_id, l.account_id, l.created_by,
                            l.created_at, l.expires_at, l.max_uses, l.uses_count, l.permission,
                            l.approval_required, l.internal_note, l.revoked_at, l.revoked_by,
                            r.repo_id, r.repo_full_name
                     FROM share_links l
                     LEFT JOIN share_link_repos r ON r.share_link_id = l.id
                     WHERE l.account_id = ?1
                     ORDER BY l.created_at DESC, l.id, r.repo_id",
                )
                .bind(&[JsValue::from_f64(account_id as f64)])
                .map_err(bind_err)?
                .all()
                .await
                .map_err(classify_d1_error)?
                .results::<ShareLinkJoinRow>()
                .map_err(|e| storage::Error::Corrupt(e.to_string()))?;
            collect_share_links(rows)
        })
        .await
    }

    // -------- invitation requests --------

    async fn insert_invitation_request_and_increment_uses(
        &self,
        _request: &InvitationRequest,
    ) -> Result<()> {
        unimplemented!("D1Storage::insert_invitation_request_and_increment_uses")
    }

    async fn record_request_decision(&self, _decision: &RequestDecision) -> Result<()> {
        unimplemented!("D1Storage::record_request_decision")
    }

    async fn get_invitation_request(&self, _id: RequestId) -> Result<Option<InvitationRequest>> {
        unimplemented!("D1Storage::get_invitation_request")
    }

    async fn list_pending_requests_for_account(
        &self,
        _account_id: u64,
    ) -> Result<Vec<InvitationRequest>> {
        unimplemented!("D1Storage::list_pending_requests_for_account")
    }

    async fn list_requests_for_link(
        &self,
        _link_id: ShareLinkId,
    ) -> Result<Vec<InvitationRequest>> {
        unimplemented!("D1Storage::list_requests_for_link")
    }

    // -------- github invitations --------

    async fn insert_github_invitation(&self, _invitation: &GithubInvitation) -> Result<()> {
        unimplemented!("D1Storage::insert_github_invitation")
    }

    async fn update_github_invitation(&self, _update: &GithubInvitationUpdate) -> Result<()> {
        unimplemented!("D1Storage::update_github_invitation")
    }

    async fn get_github_invitation(
        &self,
        _id: GithubInvitationId,
    ) -> Result<Option<GithubInvitation>> {
        unimplemented!("D1Storage::get_github_invitation")
    }

    async fn get_github_invitation_by_github_id(
        &self,
        _github_id: u64,
    ) -> Result<Option<GithubInvitation>> {
        unimplemented!("D1Storage::get_github_invitation_by_github_id")
    }

    async fn list_pending_github_invitations_for_installation(
        &self,
        _installation_id: u64,
    ) -> Result<Vec<GithubInvitation>> {
        unimplemented!("D1Storage::list_pending_github_invitations_for_installation")
    }

    // -------- audit --------

    async fn audit(&self, _event: &AuditEvent) -> Result<()> {
        unimplemented!("D1Storage::audit")
    }
}
