//! D1Storage: implements the full Storage trait using worker::D1Database.

use async_trait::async_trait;
use audit::AuditEvent;
use chrono::{DateTime, Utc};
use domain::{
    Account, GithubInvitation, GithubInvitationId, InvitationRequest, RequestId, SelectedRepos,
    ShareLink, ShareLinkId, User,
};
use storage::{GithubInvitationUpdate, RequestDecision, Result, Storage};
use worker::D1Database;

pub struct D1Storage {
    pub db: D1Database,
}

impl D1Storage {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

// wasm32 is single-threaded: no actual threads exist, so Send + Sync are vacuously true.
unsafe impl Send for D1Storage {}
unsafe impl Sync for D1Storage {}

#[async_trait]
impl Storage for D1Storage {
    // -------- installations --------

    async fn insert_installation(&self, _account: &Account) -> Result<()> {
        unimplemented!("D1Storage::insert_installation")
    }

    async fn mark_installation_uninstalled(
        &self,
        _installation_id: u64,
        _when: DateTime<Utc>,
    ) -> Result<()> {
        unimplemented!("D1Storage::mark_installation_uninstalled")
    }

    async fn update_installation_repos(
        &self,
        _installation_id: u64,
        _selected: &SelectedRepos,
    ) -> Result<()> {
        unimplemented!("D1Storage::update_installation_repos")
    }

    async fn get_installation(&self, _installation_id: u64) -> Result<Option<Account>> {
        unimplemented!("D1Storage::get_installation")
    }

    async fn get_active_installation_by_account_id(
        &self,
        _account_id: u64,
    ) -> Result<Option<Account>> {
        unimplemented!("D1Storage::get_active_installation_by_account_id")
    }

    async fn get_active_installation_by_login(&self, _login: &str) -> Result<Option<Account>> {
        unimplemented!("D1Storage::get_active_installation_by_login")
    }

    async fn list_active_installations(&self) -> Result<Vec<Account>> {
        unimplemented!("D1Storage::list_active_installations")
    }

    // -------- users --------

    async fn upsert_user(&self, _user: &User) -> Result<()> {
        unimplemented!("D1Storage::upsert_user")
    }

    async fn get_user(&self, _user_id: u64) -> Result<Option<User>> {
        unimplemented!("D1Storage::get_user")
    }

    // -------- share links --------

    async fn insert_share_link(&self, _link: &ShareLink) -> Result<()> {
        unimplemented!("D1Storage::insert_share_link")
    }

    async fn mark_share_link_revoked(
        &self,
        _id: ShareLinkId,
        _by_user: u64,
        _when: DateTime<Utc>,
    ) -> Result<()> {
        unimplemented!("D1Storage::mark_share_link_revoked")
    }

    async fn get_share_link_by_id(&self, _id: ShareLinkId) -> Result<Option<ShareLink>> {
        unimplemented!("D1Storage::get_share_link_by_id")
    }

    async fn get_share_link_by_slug(&self, _slug: &str) -> Result<Option<ShareLink>> {
        unimplemented!("D1Storage::get_share_link_by_slug")
    }

    async fn list_share_links_for_account(&self, _account_id: u64) -> Result<Vec<ShareLink>> {
        unimplemented!("D1Storage::list_share_links_for_account")
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
