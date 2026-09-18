//! D1 parameter binding helpers + conflict-error classification.
#[cfg(target_arch = "wasm32")]
pub use wasm_impl::*;

#[cfg(target_arch = "wasm32")]
pub mod wasm_impl {
    use ghinvite_core::storage::{ConflictKind, Error};
    use ghinvite_core::{
        Account, AccountType, InvitationLink, InvitationLinkId, InvitationLinkRepo, Slug, User,
    };
    use serde::Deserialize;
    use std::collections::BTreeMap;
    use std::str::FromStr;
    use ulid::Ulid;

    /// Convert a worker::Error to a ghinvite_core::storage::Error by inspecting the message string.
    pub fn classify_d1_error(e: worker::Error) -> Error {
        let msg = e.to_string();
        if msg.contains("UNIQUE constraint failed") {
            if msg.contains("invitation_links.slug") {
                return Error::Conflict(ConflictKind::DuplicateSlug);
            }
            // DuplicatePendingRequest: partial unique index covers BOTH columns.
            // Check both to avoid misclassifying PK collisions on invitation_requests.id.
            if msg.contains("invitation_requests.invitation_link_id")
                && msg.contains("invitation_requests.requester_id")
            {
                return Error::Conflict(ConflictKind::DuplicatePendingRequest);
            }
            // DuplicateActiveInstallation: partial unique index on installations.account_id
            if msg.contains("installations.account_id") {
                return Error::Conflict(ConflictKind::DuplicateActiveInstallation);
            }
            return Error::Conflict(ConflictKind::DuplicateId);
        }
        if msg.contains("FOREIGN KEY constraint failed") {
            return Error::Conflict(ConflictKind::ForeignKey);
        }
        Error::Database(msg)
    }

    // ── Row types ─────────────────────────────────────────────────────────────

    #[derive(Deserialize)]
    pub struct InstallationRow {
        pub installation_id: i64,
        pub account_id: i64,
        pub account_login: String,
        pub account_type: String,
        pub installed_at: String,
        pub uninstalled_at: Option<String>,
        pub selected_repos: String,
    }

    impl InstallationRow {
        pub fn try_into_domain(self) -> ghinvite_core::storage::Result<Account> {
            Ok(Account {
                installation_id: self.installation_id as u64,
                account_id: self.account_id as u64,
                account_login: self.account_login,
                account_type: AccountType::from_str(&self.account_type)
                    .map_err(|e| Error::Corrupt(e.to_string()))?,
                installed_at: parse_dt(&self.installed_at)?,
                uninstalled_at: self.uninstalled_at.as_deref().map(parse_dt).transpose()?,
                selected_repos: parse_selected_repos(&self.selected_repos)?,
            })
        }
    }

    #[derive(Deserialize)]
    pub struct UserRow {
        pub user_id: i64,
        pub login: String,
        pub avatar_url: Option<String>,
        pub last_seen_at: String,
    }

    impl UserRow {
        pub fn into_domain(self) -> ghinvite_core::storage::Result<User> {
            Ok(User {
                user_id: self.user_id as u64,
                login: self.login,
                avatar_url: self.avatar_url,
                last_seen_at: parse_dt(&self.last_seen_at)?,
            })
        }
    }

    #[derive(Deserialize)]
    pub struct InvitationLinkJoinRow {
        pub id: String,
        pub slug: String,
        pub installation_id: i64,
        pub account_id: i64,
        pub created_by: i64,
        pub created_at: String,
        pub expires_at: Option<String>,
        pub max_uses: Option<i64>,
        pub uses_count: i64,
        pub permission: String,
        pub approval_required: i64,
        pub description: String,
        pub internal_note: Option<String>,
        pub revoked_at: Option<String>,
        pub revoked_by: Option<i64>,
        pub repo_id: Option<i64>,
        pub repo_full_name: Option<String>,
    }

    fn row_to_invitation_link(
        row: &InvitationLinkJoinRow,
        repos: Vec<InvitationLinkRepo>,
    ) -> ghinvite_core::storage::Result<InvitationLink> {
        Ok(InvitationLink {
            id: InvitationLinkId::from_ulid(
                Ulid::from_str(&row.id).map_err(|e| Error::Corrupt(format!("link id: {e}")))?,
            ),
            slug: Slug::from_string(row.slug.clone())
                .map_err(|e| Error::Corrupt(format!("slug: {e}")))?,
            installation_id: row.installation_id as u64,
            account_id: row.account_id as u64,
            created_by: row.created_by as u64,
            created_at: parse_dt(&row.created_at)?,
            expires_at: row.expires_at.as_deref().map(parse_dt).transpose()?,
            max_uses: row.max_uses.map(|m| m as u32),
            uses_count: row.uses_count as u32,
            permission: row.permission.parse().map_err(
                |e: ghinvite_core::permission::UnknownPermission| Error::Corrupt(e.to_string()),
            )?,
            approval_required: row.approval_required != 0,
            description: row.description.clone(),
            internal_note: row.internal_note.clone(),
            revoked_at: row.revoked_at.as_deref().map(parse_dt).transpose()?,
            revoked_by: row.revoked_by.map(|r| r as u64),
            repos,
        })
    }

    /// Collapse a flat `LEFT JOIN` result into a single `InvitationLink`. Returns `None`
    /// if the result set was empty.
    pub fn try_one_invitation_link(
        rows: Vec<InvitationLinkJoinRow>,
    ) -> ghinvite_core::storage::Result<Option<InvitationLink>> {
        let mut iter = rows.into_iter();
        let Some(first) = iter.next() else {
            return Ok(None);
        };
        let mut repos = Vec::new();
        if let (Some(rid), Some(name)) = (first.repo_id, first.repo_full_name.clone()) {
            repos.push(InvitationLinkRepo {
                repo_id: rid as u64,
                repo_full_name: name,
            });
        }
        for row in iter {
            if let (Some(rid), Some(name)) = (row.repo_id, row.repo_full_name) {
                repos.push(InvitationLinkRepo {
                    repo_id: rid as u64,
                    repo_full_name: name,
                });
            }
        }
        Ok(Some(row_to_invitation_link(&first, repos)?))
    }

    /// Collapse a flat `LEFT JOIN` result into a list of `InvitationLink`s, preserving
    /// the SQL ORDER BY ordering.
    pub fn collect_invitation_links(
        rows: Vec<InvitationLinkJoinRow>,
    ) -> ghinvite_core::storage::Result<Vec<InvitationLink>> {
        // We preserve the SQL ordering (created_at DESC, id) by tracking insertion order.
        let mut order: Vec<String> = Vec::new();
        // Map from id → (first row for the link, accumulated repos)
        let mut by_link: BTreeMap<String, (usize, Vec<InvitationLinkRepo>)> = BTreeMap::new();
        // We also need to keep the first row per link for conversion.
        let mut first_rows: Vec<InvitationLinkJoinRow> = Vec::new();

        for row in rows {
            let key = row.id.clone();
            if let std::collections::btree_map::Entry::Vacant(e) = by_link.entry(key.clone()) {
                let idx = first_rows.len();
                first_rows.push(InvitationLinkJoinRow {
                    id: row.id.clone(),
                    slug: row.slug.clone(),
                    installation_id: row.installation_id,
                    account_id: row.account_id,
                    created_by: row.created_by,
                    created_at: row.created_at.clone(),
                    expires_at: row.expires_at.clone(),
                    max_uses: row.max_uses,
                    uses_count: row.uses_count,
                    permission: row.permission.clone(),
                    approval_required: row.approval_required,
                    description: row.description.clone(),
                    internal_note: row.internal_note.clone(),
                    revoked_at: row.revoked_at.clone(),
                    revoked_by: row.revoked_by,
                    repo_id: None,
                    repo_full_name: None,
                });
                e.insert((idx, Vec::new()));
                order.push(key.clone());
            }
            let (idx, repos_vec) = by_link.get_mut(&key).expect("just inserted or existed");
            if let (Some(rid), Some(name)) = (row.repo_id, row.repo_full_name) {
                repos_vec.push(InvitationLinkRepo {
                    repo_id: rid as u64,
                    repo_full_name: name,
                });
            }
            let _ = idx; // suppress unused warning
        }

        order
            .into_iter()
            .map(|k| {
                let (idx, repos) = by_link.remove(&k).expect("inserted above");
                row_to_invitation_link(&first_rows[idx], repos)
            })
            .collect()
    }

    #[derive(Deserialize)]
    pub struct InvitationRequestRow {
        pub id: String,
        pub invitation_link_id: String,
        pub requester_id: i64,
        pub justification: Option<String>,
        pub state: String,
        pub decided_by: Option<i64>,
        pub decided_at: Option<String>,
        pub decline_reason: Option<String>,
        pub created_at: String,
        pub decision_deadline: Option<String>,
    }

    impl InvitationRequestRow {
        pub fn try_into_domain(
            self,
        ) -> ghinvite_core::storage::Result<ghinvite_core::InvitationRequest> {
            use std::str::FromStr;
            Ok(ghinvite_core::InvitationRequest {
                id: ghinvite_core::RequestId::from_ulid(
                    ulid::Ulid::from_str(&self.id)
                        .map_err(|e| Error::Corrupt(format!("req id: {e}")))?,
                ),
                invitation_link_id: InvitationLinkId::from_ulid(
                    ulid::Ulid::from_str(&self.invitation_link_id)
                        .map_err(|e| Error::Corrupt(format!("link id: {e}")))?,
                ),
                requester_id: self.requester_id as u64,
                justification: self.justification,
                state: ghinvite_core::RequestState::from_str(&self.state)
                    .map_err(|e| Error::Corrupt(e.to_string()))?,
                decided_by: self.decided_by.map(|d| d as u64),
                decided_at: self.decided_at.as_deref().map(parse_dt).transpose()?,
                decline_reason: self.decline_reason,
                created_at: parse_dt(&self.created_at)?,
                decision_deadline: self
                    .decision_deadline
                    .as_deref()
                    .map(parse_dt)
                    .transpose()?,
            })
        }
    }

    #[derive(Deserialize)]
    pub struct GithubInvitationRow {
        pub id: String,
        pub invitation_request_id: String,
        pub repo_id: i64,
        pub github_invitation_id: Option<i64>,
        pub state: String,
        pub error_message: Option<String>,
        pub created_at: String,
        pub updated_at: String,
    }

    impl GithubInvitationRow {
        pub fn try_into_domain(
            self,
        ) -> ghinvite_core::storage::Result<ghinvite_core::GithubInvitation> {
            use std::str::FromStr;
            Ok(ghinvite_core::GithubInvitation {
                id: ghinvite_core::GithubInvitationId::from_ulid(
                    ulid::Ulid::from_str(&self.id)
                        .map_err(|e| Error::Corrupt(format!("ginv id: {e}")))?,
                ),
                invitation_request_id: ghinvite_core::RequestId::from_ulid(
                    ulid::Ulid::from_str(&self.invitation_request_id)
                        .map_err(|e| Error::Corrupt(format!("req id: {e}")))?,
                ),
                repo_id: self.repo_id as u64,
                github_invitation_id: self.github_invitation_id.map(|g| g as u64),
                state: ghinvite_core::InvitationState::from_str(&self.state)
                    .map_err(|e| Error::Corrupt(e.to_string()))?,
                error_message: self.error_message,
                created_at: parse_dt(&self.created_at)?,
                updated_at: parse_dt(&self.updated_at)?,
            })
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    #[derive(Deserialize)]
    pub struct AuditEventRow {
        pub id: String,
        pub account_id: u64,
        pub occurred_at: String,
        pub event_type: ghinvite_core::audit::EventType,
        pub actor_kind: ghinvite_core::audit::ActorKind,
        pub actor_id: Option<u64>,
        pub target_kind: ghinvite_core::audit::TargetKind,
        pub target_id: String,
        pub metadata: Option<String>,
        pub request_id: Option<String>,
    }

    impl AuditEventRow {
        pub fn try_into_domain(
            self,
        ) -> ghinvite_core::storage::Result<ghinvite_core::audit::AuditEvent> {
            Ok(ghinvite_core::audit::AuditEvent {
                id: self
                    .id
                    .parse()
                    .map_err(|_| Error::Corrupt("audit id".into()))?,
                account_id: self.account_id,
                occurred_at: parse_dt(&self.occurred_at)?,
                event_type: self.event_type,
                actor_kind: self.actor_kind,
                actor_id: self.actor_id,
                target_kind: self.target_kind,
                target_id: self.target_id,
                metadata: self
                    .metadata
                    .map(|m| serde_json::from_str(&m))
                    .transpose()
                    .map_err(|_| Error::Corrupt("audit metadata".into()))?
                    .unwrap_or(serde_json::Value::Null),
                request_id: self.request_id,
            })
        }
    }

    pub fn parse_dt(s: &str) -> ghinvite_core::storage::Result<chrono::DateTime<chrono::Utc>> {
        chrono::DateTime::parse_from_rfc3339(s)
            .map(|d| d.with_timezone(&chrono::Utc))
            .map_err(|e| ghinvite_core::storage::Error::Corrupt(format!("datetime: {e}")))
    }

    pub fn parse_selected_repos(
        raw: &str,
    ) -> ghinvite_core::storage::Result<ghinvite_core::SelectedRepos> {
        if raw == "all" {
            return Ok(ghinvite_core::SelectedRepos::All);
        }
        let v: Vec<u64> = serde_json::from_str(raw).map_err(|e| {
            ghinvite_core::storage::Error::Corrupt(format!("selected_repos JSON: {e}"))
        })?;
        Ok(ghinvite_core::SelectedRepos::Subset(v))
    }

    pub fn encode_selected_repos(s: &ghinvite_core::SelectedRepos) -> String {
        match s {
            ghinvite_core::SelectedRepos::All => "all".to_string(),
            ghinvite_core::SelectedRepos::Subset(v) => {
                serde_json::to_string(v).expect("vec<u64> serializes")
            }
        }
    }
}
