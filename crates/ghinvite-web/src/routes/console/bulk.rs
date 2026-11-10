//! Native bulk journey. All ingress retries use the encrypted original record;
//! current authority over every immutable target is checked on each request.
use super::{AppState, RequireConsoleAdminOf, authorize_targets, render};
use crate::{
    WebError,
    attempt_continuations::{AttemptContinuations, Retention, Scope},
    bulk_submission::{PreparedSubmission, ProtectedSubmission},
    email_rows::{EmailRowsScope, PreparedEmailRows, RawRow},
    link_authority::AuthorityError,
    middleware::csrf::CsrfForm,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};
use chrono::Utc;
use dioxus::prelude::*;
use ghinvite_core::{
    bulk::{BulkBatchId, Entry, Manifest, Review},
    bulk_commands::{
        BatchProgress, BatchQuery, CancelBatch, CreateBatch, EditBatch, ProgressCursor,
        ProgressPageQuery,
    },
    invitation_link::AccountAdmin,
    request_lifecycle::LifecycleOperationId,
    storage::email_vault::EmailVaultBinding,
};
use ghinvite_ui::link_form::{self, RepositoryChoice};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use zeroize::Zeroizing;

type BatchPath = (String, String);

#[derive(Default, Deserialize)]
pub(super) struct ProgressParams {
    after: Option<String>,
}
impl ProgressParams {
    fn cursor(&self) -> crate::Result<Option<ProgressCursor>> {
        let Some(after) = &self.after else {
            return Ok(None);
        };
        if after.len() > 40 {
            return Err(invalid("Invalid progress cursor."));
        }
        let mut parts = after.split(':');
        match (
            parts.next().and_then(|v| v.parse().ok()),
            parts.next().and_then(|v| v.parse().ok()),
            parts.next().and_then(|v| v.parse().ok()),
            parts.next(),
        ) {
            (Some(review_revision), Some(row), Some(target_offset), None) => {
                Ok(Some(ProgressCursor {
                    review_revision,
                    row,
                    target_offset,
                }))
            }
            _ => Err(invalid("Invalid progress cursor.")),
        }
    }
}

#[derive(Serialize, Deserialize)]
enum Record {
    Draft {
        // Kept inside the encrypted continuation only. Neither this digest nor
        // the address plan is copied to a workflow, URL, audit or log.
        input_digest: String,
        manifest: Manifest,
        email_plan: String,
    },
    Create(Box<CreateBatch>),
    Edit(EditBatch),
    Submit(ProtectedSubmission),
    Cancel(CancelBatch),
    DisplayClaim {
        nonce: String,
    },
}

#[derive(Deserialize, Serialize)]
pub(super) struct EntryForm {
    entries: String,
    #[serde(default)]
    repo_ids: Vec<u64>,
    #[serde(default)]
    repo_grants: Vec<String>,
    #[serde(default)]
    include_organization: Option<String>,
    #[serde(default)]
    team_ids: Vec<u64>,
    #[serde(default)]
    access_lifetime_hours: String,
}
impl Drop for EntryForm {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.entries.zeroize();
    }
}
#[derive(Deserialize)]
pub(super) struct EditForm {
    operation_id: LifecycleOperationId,
    expected_revision: u64,
    #[serde(default)]
    remove_rows: BTreeSet<u16>,
    #[serde(default)]
    personal_link_rows: BTreeSet<u16>,
}
#[derive(Deserialize)]
pub(super) struct SubmitForm {
    operation_id: LifecycleOperationId,
    expected_revision: u64,
}
#[derive(Deserialize)]
pub(super) struct CancelForm {
    operation_id: LifecycleOperationId,
}
#[derive(Deserialize)]
pub(super) struct RetryForm {
    record_id: String,
}

fn enabled(state: &AppState) -> crate::Result<()> {
    if state.config.bulk_enabled {
        Ok(())
    } else {
        Err(WebError::NotFound)
    }
}
fn actor(admin: &RequireConsoleAdminOf) -> AccountAdmin {
    AccountAdmin {
        account_id: admin.account.account_id,
        user_id: admin.session.user_id,
    }
}
fn query(admin: &RequireConsoleAdminOf, batch_id: BulkBatchId) -> BatchQuery {
    BatchQuery {
        batch_id,
        admin: actor(admin),
    }
}
fn scope(admin: &RequireConsoleAdminOf, batch: BulkBatchId) -> crate::Result<Scope> {
    Scope::bulk(
        &admin.tower,
        admin.session.user_id,
        admin.account.account_id,
        batch,
    )
}
fn email_scope(
    admin: &RequireConsoleAdminOf,
    manifest: &Manifest,
) -> crate::Result<EmailRowsScope> {
    EmailRowsScope::new(
        &admin.tower,
        manifest.batch_id,
        manifest.admin.account_id,
        manifest
            .org_installation_id()
            .unwrap_or(manifest.installation_id),
    )
    .map_err(|_| preparation_unavailable())
}
fn preparation_unavailable() -> WebError {
    WebError::Session("Bulk attempt recovery is temporarily unavailable.".into())
}
fn invalid(message: &'static str) -> WebError {
    WebError::BadRequest(message.into())
}
fn operation() -> String {
    ghinvite_core::RequestId::new().to_string()
}

/// Duplicate address rows share the original row's reference. Removing a later
/// duplicate must not clear the original's review-address record.
fn removed_email_bindings(review: &Review) -> Vec<EmailVaultBinding> {
    let mut seen = std::collections::HashSet::new();
    review
        .manifest
        .entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let Entry::Email { email_ref } = entry else {
                return None;
            };
            let row = index as u16 + 1;
            if !seen.insert(*email_ref) || !review.removed.contains(&row) {
                return None;
            }
            Some(EmailVaultBinding {
                email_ref: *email_ref,
                batch: ghinvite_core::bulk::BatchRef {
                    batch_id: review.manifest.batch_id,
                    row,
                },
                account_id: review.manifest.admin.account_id,
                installation_id: review.manifest.org_installation_id()?,
            })
        })
        .collect()
}

pub(super) async fn new(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Query(params): axum::extract::Query<BTreeMap<String, String>>,
) -> crate::Result<Response> {
    enabled(&state)?;
    let search = params.get("q").cloned().unwrap_or_default();
    let choices = admin
        .authority
        .search_repositories(&search)
        .await
        .map_err(|_| WebError::ConsoleAuthority)?;
    if choices.pending {
        return Err(WebError::ConsoleAuthority);
    }
    let repos: Vec<_> = choices
        .repositories
        .into_iter()
        .map(|r| RepositoryChoice {
            id: r.id,
            full_name: r.full_name,
        })
        .collect();
    let organization = admin
        .authority
        .organization_offer()
        .await
        .map_err(|_| WebError::ConsoleAuthority)?;
    let signed = Some(admin.session.login.clone());
    let login = admin.account.account_login.clone();
    let batch = BulkBatchId::new().to_string();
    let access_removal_enabled = state.config.access_removal_enabled;
    let html = render(admin.render_context(), move || {
        rsx! {
            ghinvite_ui::bulk::BulkEntryPage { signed_in_login:signed.clone(),account_login:login.clone(),batch_id:batch.clone(),repos:repos.clone(),organization:organization.clone(),access_removal_enabled,repository_query:search.clone() }
        }
    });
    Ok(Html(html).into_response())
}

/// Count before any resolver, target lookup, vault or entropy operation. Empty
/// lines do not create rows; permanent row numbers follow the submitted entries.
fn raw_rows(value: &str) -> crate::Result<Vec<RawRow>> {
    let values: Vec<_> = value
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .take(201)
        .collect();
    if values.is_empty() || values.len() > ghinvite_core::bulk::MAX_ROWS {
        return Err(invalid("Enter between one and 200 rows."));
    }
    Ok(values
        .into_iter()
        .enumerate()
        .map(|(i, value)| RawRow {
            row: i as u16 + 1,
            value: Zeroizing::new(value.to_owned()),
        })
        .collect())
}

async fn can_create(admin: &RequireConsoleAdminOf, manifest: &Manifest) -> crate::Result<()> {
    if manifest.admin.account_id != admin.account.account_id {
        return Err(WebError::NotFound);
    }
    match admin.authority.can_create_targets(&manifest.targets).await {
        Ok(crate::authority::Access::Allowed) => {}
        Ok(crate::authority::Access::Denied) => return Err(WebError::Forbidden),
        _ => return Err(WebError::ConsoleAuthority),
    }
    if admin
        .authority
        .creation_installations(&manifest.targets)
        .map_err(|_| WebError::ConsoleAuthority)?
        != (
            manifest.installation_id,
            manifest.organization_installation_id,
        )
    {
        return Err(invalid(
            "The installation selection changed. Start a new batch.",
        ));
    }
    Ok(())
}

pub(super) async fn resolve(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    Path((_, batch)): Path<BatchPath>,
    CsrfForm(form): CsrfForm<EntryForm>,
) -> crate::Result<Response> {
    let batch: BulkBatchId = batch.parse().map_err(|_| WebError::NotFound)?;
    use sha2::{Digest, Sha256};
    enabled(&state)?;
    let rows = raw_rows(&form.entries)?;
    let input = Zeroizing::new(serde_json::to_vec(&form).map_err(|_| invalid("Invalid entries."))?);
    let input_digest = hex::encode(Sha256::digest(&*input));
    let store = AttemptContinuations::new(&state);
    let scope = scope(&admin, batch)?;
    let draft = if let Some(original) = store.load::<Record>(&scope, "draft").await? {
        match &original {
            Record::Draft {
                input_digest: old, ..
            } if old == &input_digest => original,
            _ => return Ok(conflict(&admin)),
        }
    } else {
        if form
            .include_organization
            .as_deref()
            .is_some_and(|v| v != "true")
        {
            return Err(invalid("Invalid organization selection."));
        }
        let choices = admin
            .authority
            .repositories_for_review(&form.repo_ids)
            .await
            .map_err(|_| WebError::ConsoleAuthority)?;
        if choices.pending {
            return Err(WebError::ConsoleAuthority);
        }
        let repos: Vec<_> = choices
            .repositories
            .into_iter()
            .map(|r| RepositoryChoice {
                id: r.id,
                full_name: r.full_name,
            })
            .collect();
        let organization = admin
            .authority
            .organization_offer()
            .await
            .map_err(|_| WebError::ConsoleAuthority)?;
        let values = link_form::LinkFormValues {
            selected_repo_ids: form.repo_ids.clone(),
            repo_grants: form.repo_grants.clone(),
            selected_team_ids: form.team_ids.clone(),
            include_organization: form.include_organization.is_some(),
            ..Default::default()
        };
        let (model, _) = values.to_model();
        let targets =
            link_form::selected_targets(&model, &repos, &organization).map_err(invalid)?;
        let access_lifetime_seconds =
            link_form::access_lifetime_seconds(&form.access_lifetime_hours).map_err(invalid)?;
        if access_lifetime_seconds.is_some() && !state.config.access_removal_enabled {
            return Err(invalid("Access lifetime is unavailable."));
        }
        let (installation_id, organization_installation_id) = admin
            .authority
            .creation_installations(&targets)
            .map_err(|_| WebError::ConsoleAuthority)?;
        let emails = PreparedEmailRows::prepare(
            batch,
            admin.account.account_id,
            organization_installation_id.unwrap_or(installation_id),
            rows,
        )
        .map_err(|error| WebError::BadRequest(error.to_string()))?;
        if emails
            .entries()
            .iter()
            .any(|entry| matches!(entry, Entry::Email { .. }))
            && state.bulk_email_vault.is_none()
        {
            return Err(invalid("Email organization invitations are unavailable."));
        }
        let manifest = Manifest {
            batch_id: batch,
            admin: actor(&admin),
            installation_id,
            organization_installation_id,
            targets,
            access_lifetime_seconds,
            entries: emails.entries().to_vec(),
        }
        .normalize()
        .map_err(|_| invalid("Invalid batch selection."))?;
        can_create(&admin, &manifest).await?;
        let email_plan = emails
            .seal(
                &state.config.keyring,
                &email_scope(&admin, &manifest)?,
                Utc::now().timestamp(),
            )
            .map_err(|_| preparation_unavailable())?;
        let draft = Record::Draft {
            input_digest: input_digest.clone(),
            manifest,
            email_plan,
        };
        match store.retain(&scope, "draft", None, &draft).await? {
            Retention::Retained => draft,
            Retention::Conflict(original) | Retention::Bound(original) => match &original {
                Record::Draft {
                    input_digest: old, ..
                } if old == &input_digest => original,
                _ => return Ok(conflict(&admin)),
            },
        }
    };
    create_from_draft(&state, &admin, batch, draft).await
}

async fn create_from_draft(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    batch: BulkBatchId,
    draft: Record,
) -> crate::Result<Response> {
    let Record::Draft { manifest, .. } = draft else {
        return Err(preparation_unavailable());
    };
    let store = AttemptContinuations::new(state);
    let scope = scope(admin, batch)?;
    let record = if let Some(record) = store.load::<Record>(&scope, "create").await? {
        record
    } else {
        can_create(admin, &manifest).await?;
        let evidence = state
            .bulk_authority
            .review_service(admin.authority.clone())
            .review(manifest.clone(), &actor(admin))
            .await
            .map_err(|_| WebError::ConsoleAuthority)?;
        let review = Review::new(manifest, evidence, Utc::now())
            .map_err(|_| invalid("The entries could not be reviewed."))?;
        let record = Record::Create(Box::new(CreateBatch {
            operation_id: batch
                .to_string()
                .try_into()
                .map_err(|_| invalid("Invalid batch identity."))?,
            review,
        }));
        match store.retain(&scope, "create", None, &record).await? {
            Retention::Retained => record,
            Retention::Conflict(original) | Retention::Bound(original) => original,
        }
    };
    execute(state, admin, batch, "create", record).await
}

async fn authoritative(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    batch: BulkBatchId,
    mutation: bool,
) -> crate::Result<Review> {
    enabled(state)?;
    let review = state.bulk_authority.review(query(admin, batch)).await?;
    review.validate().map_err(|_| preparation_unavailable())?;
    if review.manifest.batch_id != batch
        || review.manifest.admin.account_id != admin.account.account_id
    {
        return Err(WebError::NotFound);
    }
    authorize_targets(admin, &review.manifest.targets, mutation).await?;
    Ok(review)
}

pub(super) async fn detail(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    Path((_, batch)): Path<BatchPath>,
    axum::extract::Query(params): axum::extract::Query<ProgressParams>,
) -> crate::Result<Response> {
    let batch: BulkBatchId = batch.parse().map_err(|_| WebError::NotFound)?;
    enabled(&state)?;
    let cursor = params.cursor()?;
    match authoritative(&state, &admin, batch, false).await {
        Ok(review) => page_with_cursor(&state, &admin, review, vec![], None, cursor).await,
        Err(WebError::NotFound) => {
            let store = AttemptContinuations::new(&state);
            let scope = scope(&admin, batch)?;
            if let Some(Record::Draft { manifest, .. }) =
                store.load::<Record>(&scope, "draft").await?
            {
                authorize_targets(&admin, &manifest.targets, false).await?;
                Ok(recovery(
                    &admin,
                    batch,
                    "draft",
                    "Continue resolving this batch with its original entries.",
                ))
            } else {
                Err(WebError::NotFound)
            }
        }
        Err(error) => Err(error),
    }
}

pub(super) async fn edit(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    Path((_, batch)): Path<BatchPath>,
    CsrfForm(form): CsrfForm<EditForm>,
) -> crate::Result<Response> {
    let batch: BulkBatchId = batch.parse().map_err(|_| WebError::NotFound)?;
    let review = authoritative(&state, &admin, batch, true).await?;
    let command = EditBatch {
        query: query(&admin, batch),
        operation_id: form.operation_id,
        expected_revision: form.expected_revision,
        remove: form.remove_rows,
        personal_link_rows: form.personal_link_rows,
    };
    let record_id = format!("edit-{}", String::from(command.operation_id.clone()));
    // Owner validation retains exact refusals too; do not turn a retry into a
    // new locally edited snapshot after the review has advanced.
    let _ = review;
    retain_execute(&state, &admin, batch, &record_id, Record::Edit(command)).await
}

pub(super) async fn submit(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    Path((_, batch)): Path<BatchPath>,
    CsrfForm(form): CsrfForm<SubmitForm>,
) -> crate::Result<Response> {
    let batch: BulkBatchId = batch.parse().map_err(|_| WebError::NotFound)?;
    let review = authoritative(&state, &admin, batch, true).await?;
    let store = AttemptContinuations::new(&state);
    let scope = scope(&admin, batch)?;
    let record = if let Some(record) = store.load::<Record>(&scope, "submit").await? {
        match &record {
            Record::Submit(original)
                if original.command.operation_id == form.operation_id
                    && original.command.expected_revision == form.expected_revision =>
            {
                record
            }
            _ => return Ok(conflict(&admin)),
        }
    } else {
        if review.revision != form.expected_revision {
            return Ok(conflict(&admin));
        }
        let prepared =
            PreparedSubmission::new(&review, actor(&admin), form.operation_id, Utc::now())
                .map_err(|_| {
                    invalid("Apply review choices and remove unresolved rows before submitting.")
                })?;
        let protected = prepared
            .protect(
                &state.config.keyring,
                &email_scope(&admin, &review.manifest)?,
                Utc::now().timestamp(),
            )
            .map_err(|_| preparation_unavailable())?;
        let record = Record::Submit(protected);
        match store.retain(&scope, "submit", None, &record).await? {
            Retention::Retained => record,
            Retention::Conflict(_) | Retention::Bound(_) => return Ok(conflict(&admin)),
        }
    };
    execute(&state, &admin, batch, "submit", record).await
}

pub(super) async fn cancel(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    Path((_, batch)): Path<BatchPath>,
    CsrfForm(form): CsrfForm<CancelForm>,
) -> crate::Result<Response> {
    let batch: BulkBatchId = batch.parse().map_err(|_| WebError::NotFound)?;
    authoritative(&state, &admin, batch, true).await?;
    let command = CancelBatch {
        query: query(&admin, batch),
        operation_id: form.operation_id,
    };
    let record_id = format!("cancel-{}", String::from(command.operation_id.clone()));
    retain_execute(&state, &admin, batch, &record_id, Record::Cancel(command)).await
}

async fn retain_execute(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    batch: BulkBatchId,
    id: &str,
    record: Record,
) -> crate::Result<Response> {
    let store = AttemptContinuations::new(state);
    let scope = scope(admin, batch)?;
    match store.retain(&scope, id, None, &record).await? {
        Retention::Retained => execute(state, admin, batch, id, record).await,
        Retention::Conflict(_) | Retention::Bound(_) => Ok(conflict(admin)),
    }
}

pub(super) async fn retry(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    Path((_, batch)): Path<BatchPath>,
    CsrfForm(form): CsrfForm<RetryForm>,
) -> crate::Result<Response> {
    let batch: BulkBatchId = batch.parse().map_err(|_| WebError::NotFound)?;
    enabled(&state)?;
    let record = AttemptContinuations::new(&state)
        .load::<Record>(&scope(&admin, batch)?, &form.record_id)
        .await?
        .ok_or(WebError::NotFound)?;
    if form.record_id == "draft" {
        create_from_draft(&state, &admin, batch, record).await
    } else {
        execute(&state, &admin, batch, &form.record_id, record).await
    }
}

async fn execute(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    batch: BulkBatchId,
    id: &str,
    record: Record,
) -> crate::Result<Response> {
    enabled(state)?;
    let result = match record {
        Record::Create(command) => {
            if command.review.manifest.batch_id != batch
                || command.review.manifest.admin != actor(admin)
            {
                return Err(WebError::NotFound);
            }
            // Recover the exact accepted operation under historical authority.
            // Availability only gates new creation; it cannot hide a retained
            // receipt after uninstall or force a new allocation.
            match state.bulk_authority.review(query(admin, batch)).await {
                Ok(existing) if existing.manifest == command.review.manifest => {
                    authorize_targets(admin, &existing.manifest.targets, true).await?;
                }
                Ok(_) => return Ok(conflict(admin)),
                Err(AuthorityError::Missing) => {
                    can_create(admin, &command.review.manifest).await?;
                    if command
                        .review
                        .manifest
                        .entries
                        .iter()
                        .any(|row| matches!(row, Entry::Email { .. }))
                    {
                        let draft = AttemptContinuations::new(state)
                            .load::<Record>(&scope(admin, batch)?, "draft")
                            .await?;
                        let Some(Record::Draft {
                            manifest,
                            email_plan,
                            ..
                        }) = draft
                        else {
                            return Ok(expired_review(admin, batch, id));
                        };
                        if manifest != command.review.manifest {
                            return Err(preparation_unavailable());
                        }
                        let rows = match PreparedEmailRows::restore(
                            &email_plan,
                            &state.config.keyring,
                            &email_scope(admin, &manifest)?,
                            Utc::now().timestamp(),
                        ) {
                            Ok(rows) => rows,
                            Err(_) => return Ok(expired_review(admin, batch, id)),
                        };
                        if (Manifest {
                            entries: rows.entries().to_vec(),
                            ..manifest.clone()
                        })
                        .normalize()
                        .ok()
                        .as_ref()
                            != Some(&manifest)
                        {
                            return Err(preparation_unavailable());
                        }
                        if rows
                            .retain_review(
                                state
                                    .bulk_email_vault
                                    .as_ref()
                                    .ok_or_else(preparation_unavailable)?,
                                Utc::now().timestamp(),
                            )
                            .await
                            .is_err()
                        {
                            return Ok(expired_review(admin, batch, id));
                        }
                    }
                }
                Err(AuthorityError::Unknown(_)) => {
                    return Ok(recovery(
                        admin,
                        batch,
                        id,
                        "The batch review could not be confirmed. Retry the original operation to recover its result.",
                    ));
                }
                Err(error) => return Err(error.into()),
            }
            state.bulk_authority.create(*command).await
        }
        Record::Edit(command) => {
            authoritative(state, admin, batch, true).await?;
            if command.query != query(admin, batch) {
                return Err(WebError::NotFound);
            }
            match state.bulk_authority.remove_rows(command).await {
                Ok(review) => {
                    let bindings = removed_email_bindings(&review);
                    if !bindings.is_empty()
                        && state
                            .bulk_email_vault
                            .as_ref()
                            .ok_or_else(preparation_unavailable)?
                            .retire_removed(&bindings, Utc::now().timestamp())
                            .await
                            .is_err()
                    {
                        return Ok(recovery(
                            admin,
                            batch,
                            id,
                            "The review choices were recorded. Protected data cleanup could not be confirmed; retry this original operation.",
                        ));
                    }
                    Ok(review)
                }
                Err(error) => Err(error),
            }
        }
        Record::Submit(protected) => {
            return submit_original(state, admin, batch, id, protected).await;
        }
        Record::Cancel(command) => {
            authoritative(state, admin, batch, true).await?;
            if command.query != query(admin, batch) {
                return Err(WebError::NotFound);
            }
            match state.bulk_authority.cancel_remaining(command).await {
                Ok(_) => return page(
                    state,
                    admin,
                    authoritative(state, admin, batch, false).await?,
                    vec![],
                    Some(
                        "Cancellation was recorded. Sent and uncertain deliveries remain tracked."
                            .into(),
                    ),
                )
                .await,
                Err(error) => Err(error),
            }
        }
        _ => return Err(WebError::NotFound),
    };
    match result {
        Ok(receipt) => {
            // Operation receipts are immutable. Another administrator may have
            // edited or submitted the batch since this operation was recorded.
            // A successful replay therefore proves the operation, not the
            // current review state that controls the page's actions and copy.
            receipt.validate().map_err(|_| preparation_unavailable())?;
            let current = match authoritative(state, admin, batch, false).await {
                Ok(current)
                    if current.manifest == receipt.manifest
                        && current.revision >= receipt.revision
                        && (current.revision != receipt.revision || current == receipt) =>
                {
                    current
                }
                Ok(_) | Err(WebError::Restate(_)) => {
                    return Ok(recovery(
                        admin,
                        batch,
                        id,
                        "The operation was recorded. The current batch state could not be confirmed; retry this original operation or view the batch.",
                    ));
                }
                Err(error) => return Err(error),
            };
            page(state, admin, current, vec![], None).await
        }
        Err(AuthorityError::Unknown(_)) => Ok(recovery(
            admin,
            batch,
            id,
            "The outcome is unknown. Retry the original operation to recover its result.",
        )),
        Err(error) => Err(error.into()),
    }
}

async fn submit_original(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    batch: BulkBatchId,
    id: &str,
    protected: ProtectedSubmission,
) -> crate::Result<Response> {
    let current = authoritative(state, admin, batch, true).await?;
    if protected.command.query != query(admin, batch) {
        return Err(WebError::NotFound);
    }
    // Shared review recovery belongs to the account. Another current admin can
    // stage exactly the submitted rows without reading the creator's session.
    if current
        .submitted
        .as_ref()
        .is_some_and(|submission| submission != &protected.command.submission)
    {
        return Ok(conflict(admin));
    }
    if current.submitted.is_none()
        && !protected.command.submission.emails.is_empty()
        && state
            .bulk_email_vault
            .as_ref()
            .ok_or_else(preparation_unavailable)?
            .stage_reviewed(&protected.command.submission.emails, Utc::now().timestamp())
            .await
            .is_err()
    {
        return Ok(expired_review(admin, batch, id));
    }
    let review = match state.bulk_authority.submit(protected.command.clone()).await {
        Ok(review) => review,
        Err(AuthorityError::Unknown(_)) => {
            return Ok(recovery(
                admin,
                batch,
                id,
                "Submission may have been accepted. Retry this original operation; it will not allocate another set of invitations.",
            ));
        }
        Err(error) => return Err(error.into()),
    };
    if review.submitted.as_ref() != Some(&protected.command.submission) {
        return Err(preparation_unavailable());
    }
    for personal in &protected.command.submission.personal_links {
        let command = personal
            .creation_for_admin(
                &review.manifest,
                protected.command.submission.submitted_at,
                &protected.command.query.admin,
            )
            .map_err(|_| preparation_unavailable())?;
        match state.link_authority.create(command).await {
            Ok(Ok(created)) if created.link_id==personal.link_id && created.secret_generation==1=>{},
            Ok(Ok(_))=>return Err(preparation_unavailable()),
            Ok(Err(_))=>return page(state,admin,review,vec![],Some("The batch was submitted, but a personal link could not be confirmed. Retry this original operation to check again.".into())).await,
            Err(AuthorityError::Unknown(_))=>return Ok(recovery(admin,batch,id,"The batch was submitted. A personal link is still awaiting confirmation; retry this original operation.")),
            Err(error)=>return Err(error.into()),
        }
    }
    // Decrypt before the claim, but only render after winning its atomic first
    // writer binding. Parallel POSTs and browser back/retry cannot redisplay.
    let mut shown = vec![];
    let store = AttemptContinuations::new(state);
    let scope = scope(admin, batch)?;
    let displayed = store.load::<Record>(&scope, "display").await?.is_some();
    if !displayed && !protected.personal.is_empty() {
        let links = match protected
            .restore_personal_links(
                &state.config.keyring,
                &email_scope(admin, &review.manifest)?,
                Utc::now().timestamp(),
            ) {
                Ok(links)=>links,
                Err(_)=>return page(state,admin,review,vec![],Some("The batch was submitted. Personal URLs could not be restored in this session. Retry this original operation, or open the personal link records below to rotate any URL you did not save.".into())).await,
            };
        if matches!(
            store
                .retain(
                    &scope,
                    "display",
                    None,
                    &Record::DisplayClaim { nonce: operation() }
                )
                .await?,
            Retention::Retained
        ) {
            shown = links
                .into_iter()
                .map(|link| (link.plan().source.row, link.show_once().to_string()))
                .collect();
        }
    }
    page(
        state,
        admin,
        review,
        shown,
        Some("Batch submitted. Check delivery progress below.".into()),
    )
    .await
}

async fn page(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    review: Review,
    shown: Vec<(u16, String)>,
    message: Option<String>,
) -> crate::Result<Response> {
    page_with_cursor(state, admin, review, shown, message, None).await
}

async fn page_with_cursor(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    review: Review,
    shown: Vec<(u16, String)>,
    message: Option<String>,
    cursor: Option<ProgressCursor>,
) -> crate::Result<Response> {
    let batch = review.manifest.batch_id;
    let store = AttemptContinuations::new(state);
    let scope = scope(admin, batch)?;
    let mut labels = BTreeMap::new();
    if let Some(Record::Draft {
        manifest,
        email_plan,
        ..
    }) = store.load::<Record>(&scope, "draft").await?
        && manifest == review.manifest
        && let Ok(rows) = PreparedEmailRows::restore(
            &email_plan,
            &state.config.keyring,
            &email_scope(admin, &manifest)?,
            Utc::now().timestamp(),
        )
    {
        labels = (1..=manifest.entries.len())
            .filter_map(|row| {
                rows.review_address(row as u16)
                    .map(|email| (row as u16, email.as_str().to_owned()))
            })
            .collect();
    }
    let progress: Option<BatchProgress> = if review.submitted.is_some() {
        match state
            .bulk_authority
            .progress_page(ProgressPageQuery {
                query: query(admin, batch),
                cursor,
            })
            .await
        {
            Ok(progress) => Some(progress),
            Err(AuthorityError::Unknown(_)) => None,
            Err(error) => return Err(error.into()),
        }
    } else {
        None
    };
    let retry_submit = store.load::<Record>(&scope, "submit").await?.is_some();
    let signed = Some(admin.session.login.clone());
    let login = admin.account.account_login.clone();
    let cursor_href = |cursor: ProgressCursor| {
        format!(
            "/console/accounts/{login}/bulk/{batch}?after={}:{}:{}",
            cursor.review_revision, cursor.row, cursor.target_offset
        )
    };
    let progress_next = progress
        .as_ref()
        .and_then(|page| page.next_cursor)
        .map(cursor_href);
    let progress_refresh = cursor
        .map(cursor_href)
        .unwrap_or_else(|| format!("/console/accounts/{login}/bulk/{batch}"));
    let edit_operation = operation();
    let submit_operation = operation();
    let cancel_operation = operation();
    let reissue_enabled = state.config.reissue_enabled;
    let organization_reissue_enabled = state.config.organization_delivery_enabled;
    let html = render(admin.render_context(), move || {
        rsx! {
            ghinvite_ui::bulk::BulkReviewPage {signed_in_login:signed.clone(),account_login:login.clone(),review:review.clone(),edit_operation:edit_operation.clone(),submit_operation:submit_operation.clone(),cancel_operation:cancel_operation.clone(),progress:progress.clone(),message:message.clone(),shown_links:shown.clone(),email_labels:labels.clone(),recover_submission:retry_submit,reissue_enabled,organization_reissue_enabled,progress_next:progress_next.clone(),progress_refresh:progress_refresh.clone()}
        }
    });
    Ok(Html(html).into_response())
}

fn conflict(admin: &RequireConsoleAdminOf) -> Response {
    let signed = Some(admin.session.login.clone());
    let login = admin.account.account_login.clone();
    let html = render(admin.render_context(), move || {
        rsx! {
            ghinvite_ui::layouts::ConsoleLayout {signed_in_login:signed.clone(),title:"Bulk operation conflict",account_login:Some(login.clone()),children:rsx!{
                h1 {"Bulk operation conflict"} p {"This operation is already bound to different input. Return to the batch to recover its original operation or start a new batch."}
            }}
        }
    });
    (StatusCode::CONFLICT, Html(html)).into_response()
}
fn recovery(
    admin: &RequireConsoleAdminOf,
    batch: BulkBatchId,
    id: &str,
    message: &str,
) -> Response {
    let signed = Some(admin.session.login.clone());
    let login = admin.account.account_login.clone();
    let id = id.to_owned();
    let message = message.to_owned();
    let html = render(admin.render_context(), move || {
        rsx! {
            ghinvite_ui::layouts::ConsoleLayout {signed_in_login:signed.clone(),title:"Recover bulk operation",account_login:Some(login.clone()),children:rsx!{
                h1 {"Recover bulk operation"} p {role:"status","{message}"}
                form {method:"post",action:format!("/console/accounts/{login}/bulk/{batch}/retry"),
                    ghinvite_ui::csrf::CsrfField {}
                    input {r#type:"hidden",name:"record_id",value:id.clone()}
                    button {r#type:"submit","Retry original operation"}
                }
                a {href:format!("/console/accounts/{login}/bulk/{batch}"),"View batch"}
            a {href:format!("/console/accounts/{login}/bulk/new"),"Start another batch"}
            }}
        }
    });
    (StatusCode::SERVICE_UNAVAILABLE, Html(html)).into_response()
}

fn expired_review(admin: &RequireConsoleAdminOf, batch: BulkBatchId, id: &str) -> Response {
    let mut response = recovery(
        admin,
        batch,
        id,
        "The address review has expired or could not be restored. Retry the original operation if recovery becomes available, or start a new batch to review the entries again.",
    );
    *response.status_mut() = StatusCode::CONFLICT;
    response
}
