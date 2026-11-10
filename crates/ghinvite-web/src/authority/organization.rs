use super::*;
use ghinvite_ui::link_form::{OrganizationChoice, OrganizationOffer, TeamChoice};

impl AuthorityView {
    /// Current app binding for advisory reads; this grants no additional authority.
    pub fn current_org_installation_id(&self) -> Option<u64> {
        self.org
            .as_ref()
            .filter(|installation| !unavailable(installation))
            .map(|installation| installation.account.installation_id)
    }
    /// Resolve installation bindings from verified app-scoped facts, never form fields.
    pub fn creation_installations(
        &self,
        targets: &Targets,
    ) -> Result<(u64, Option<u64>), AuthorityError> {
        let repo = self
            .repo
            .as_ref()
            .filter(|i| !unavailable(i))
            .map(|i| i.account.installation_id);
        let org = self
            .org
            .as_ref()
            .filter(|i| !unavailable(i))
            .map(|i| i.account.installation_id);
        if targets.organization.is_some() {
            if !self.organization_delivery_enabled {
                return Err(AuthorityError::Denied);
            }
            let org = org.ok_or(AuthorityError::Denied)?;
            if targets.repos.is_empty() {
                Ok((org, None))
            } else {
                Ok((repo.ok_or(AuthorityError::Denied)?, Some(org)))
            }
        } else {
            Ok((repo.ok_or(AuthorityError::Denied)?, None))
        }
    }
    pub async fn organization_offer(&self) -> Result<OrganizationOffer, AuthorityError> {
        if self.account.account_type != AccountType::Organization {
            return Ok(OrganizationOffer::NotApplicable);
        }
        let Some(org) = &self.org else {
            return Ok(OrganizationOffer::MissingApp);
        };
        if !self.organization_delivery_enabled {
            return Ok(OrganizationOffer::Disabled);
        }
        if self.role != Role::AccountAdmin || unavailable(org) {
            return Ok(OrganizationOffer::Unavailable);
        }
        self.verify_organization().await?;
        let mut teams = Vec::new();
        let mut ids = BTreeSet::new();
        let mut slugs = BTreeSet::new();
        let mut cursor = None;
        let mut seen = Vec::new();
        loop {
            let key = format!(
                "authz:org:{}:0:teams-{}",
                org.account.installation_id,
                seen.len()
            );
            let value = self
                .cached_query(
                    &key,
                    org,
                    Query::Teams(self.account.account_login.clone(), cursor.clone()),
                )
                .await?
                .ok_or(AuthorityError::Unavailable)?;
            let page: endpoints::authority_teams::TeamPage = decode(value)?;
            for team in page.teams {
                if team.id == 0
                    || team.id > 9_007_199_254_740_991
                    || team.slug.is_empty()
                    || team
                        .slug
                        .chars()
                        .any(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                    || team.name.is_empty()
                    || team.name.len() > 200
                    || team.name.chars().any(char::is_control)
                    || !ids.insert(team.id)
                    || !slugs.insert(team.slug.clone())
                    || teams.len() >= 5000
                {
                    return Err(AuthorityError::Unavailable);
                }
                teams.push(TeamChoice {
                    id: team.id,
                    slug: team.slug,
                    name: team.name,
                    externally_managed: team.ldap_dn.is_some_and(|value| !value.is_empty()),
                });
            }
            match page.next {
                None => break,
                Some(next) => {
                    if seen.contains(&next) || seen.len() >= 49 {
                        return Err(AuthorityError::Unavailable);
                    }
                    seen.push(next.clone());
                    cursor = Some(next);
                }
            }
        }
        Ok(OrganizationOffer::Available(OrganizationChoice {
            id: self.account.account_id,
            login: self.account.account_login.clone(),
            teams,
        }))
    }
    async fn verify_organization(&self) -> Result<(), AuthorityError> {
        let org = self
            .org
            .as_ref()
            .filter(|org| !unavailable(org))
            .ok_or(AuthorityError::Denied)?;
        let key = format!("authz:org:{}:0:identity", org.account.installation_id);
        let value = self
            .cached_query(
                &key,
                org,
                Query::Organization(self.account.account_login.clone()),
            )
            .await?
            .ok_or(AuthorityError::Denied)?;
        let identity: ghinvite_github::payloads::GhUser = decode(value)?;
        if identity.id != self.account.account_id
            || !identity
                .login
                .eq_ignore_ascii_case(&self.account.account_login)
        {
            return Err(AuthorityError::Denied);
        }
        Ok(())
    }
    pub(super) async fn organization_access(
        &self,
        targets: &Targets,
        creation: bool,
    ) -> Result<Access, AuthorityError> {
        if targets.validate().is_err() {
            return Ok(Access::Denied);
        }
        if creation && !self.organization_delivery_enabled {
            return Ok(Access::Denied);
        }
        let Some(target) = &targets.organization else {
            return Ok(Access::Denied);
        };
        if self.role != Role::AccountAdmin
            || target.org_id != self.account.account_id
            || !target
                .org_login
                .eq_ignore_ascii_case(&self.account.account_login)
            || self.org.as_ref().is_none_or(unavailable)
        {
            return Ok(Access::Denied);
        }
        self.verify_organization().await?;
        if !creation {
            return Ok(Access::Allowed);
        }
        let org = self.org.as_ref().ok_or(AuthorityError::Denied)?;
        for target in &targets.teams {
            let key = format!(
                "authz:org:{}:0:team-{}",
                org.account.installation_id, target.team_id
            );
            let Some(value) = self
                .cached_query(
                    &key,
                    org,
                    Query::Team(self.account.account_login.clone(), target.team_slug.clone()),
                )
                .await?
            else {
                return Ok(Access::Denied);
            };
            let identity: endpoints::authority_teams::TeamIdentity = decode(value)?;
            if identity.organization.id != self.account.account_id
                || identity.team.id != target.team_id
                || identity.team.slug != target.team_slug
                || identity.team.ldap_dn.is_some_and(|value| !value.is_empty())
            {
                return Ok(Access::Denied);
            }
        }
        Ok(Access::Allowed)
    }
}
