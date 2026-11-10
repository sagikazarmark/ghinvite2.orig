//! GitHub App permissions and the validated set an installation token requests.
//!
//! # Sources
//!
//! 1. The GitHub App settings form (`integration[default_permissions][<key>]`
//!    fields and their access menus; page saved 2026-10-18). Every permission
//!    shown there takes its key, section, title, description and access levels
//!    from the form.
//! 2. For permissions the form did not show (marked "Not on the settings
//!    page"), in order of precedence: the fine-grained permission tables in
//!    GitHub's docs ("Managing your personal access tokens"); the REST API
//!    description's `app-permissions` schema (the request body of
//!    `POST /app/installations/{id}/access_tokens`, api.github.com and GHEC);
//!    GitHub's per-endpoint permission data (`github/docs`,
//!    `src/github-apps/data/*/server-to-server-permissions.json`), where levels
//!    are only those some endpoint requires.
//!
//! Octokit was checked too (2026-10-18). `octokit/openapi`, the generated SDKs
//! and `actions/create-github-app-token` copy the REST API description;
//! `octokit/webhooks` records real payloads; `octokit.net` and
//! `octokit/app-permissions` are stale subsets. None adds a permission.
//!
//! Where the REST API description disagrees with the form (`email_addresses`
//! and `git_ssh_keys` vs `emails` and `keys`; `metadata: write`), the form wins.

use crate::error::ConfigError;
use std::collections::BTreeMap;

/// Access level for one permission. `write` includes `read`; `admin` includes
/// `write`. Not every permission supports every level. Serializes as GitHub's
/// string (`"read"`, `"write"`, `"admin"`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(u8)]
pub enum Access {
    /// `read`.
    Read,

    /// `write`.
    Write,

    /// `admin`.
    Admin,
}

impl serde::Serialize for Access {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl Access {
    /// GitHub's JSON value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Admin => "admin",
        }
    }
}

macro_rules! permission_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $( $variant:ident = $key:literal, $title:literal, [$($access:ident),+], $description:literal; )+
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
        #[non_exhaustive]
        pub enum $name {
            $(
                #[doc = concat!("**", $title, "** (`", $key, "`): ", $description)]
                #[doc = ""]
                #[doc = concat!("Access: ", stringify!($($access),+), ".")]
                $variant,
            )+
        }

        impl $name {
            /// Every permission, in the order GitHub's settings page lists them.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// GitHub's JSON key.
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $key,)+ }
            }

            /// The name on GitHub's settings page.
            pub const fn title(self) -> &'static str {
                match self { $(Self::$variant => $title,)+ }
            }

            /// The help text on GitHub's settings page.
            pub const fn description(self) -> &'static str {
                match self { $(Self::$variant => $description,)+ }
            }

            /// The access levels GitHub accepts for this permission.
            pub const fn access_levels(self) -> &'static [Access] {
                match self { $(Self::$variant => &[$(Access::$access),+],)+ }
            }

            /// Whether GitHub accepts `access` for this permission.
            pub const fn accepts(self, access: Access) -> bool {
                let levels = self.access_levels();
                let mut i = 0;
                while i < levels.len() {
                    if levels[i] as u8 == access as u8 {
                        return true;
                    }
                    i += 1;
                }
                false
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        /// Serializes as GitHub's JSON key.
        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }
    };
}

permission_enum! {
    /// Repository permissions permit access to repositories and related resources.
    pub enum RepositoryPermission {
        Actions = "actions", "Actions", [Read, Write],
            "Workflows, workflow runs and artifacts.";
        Administration = "administration", "Administration", [Read, Write],
            "Repository creation, deletion, settings, teams, and collaborators.";
        AgentSecrets = "agent_secrets", "Agent secrets", [Read, Write],
            "Manage agent repository secrets.";
        AgentTasks = "agent_tasks", "Agent tasks", [Read, Write],
            "View, create, and manage agent tasks in a repository.";
        AgentVariables = "agent_variables", "Agent variables", [Read, Write],
            "Manage agent repository variables.";
        ArtifactMetadata = "artifact_metadata", "Artifact metadata", [Read, Write],
            "Create and retrieve artifact metadata for a repository.";
        Attestations = "attestations", "Attestations", [Read, Write],
            "Create and retrieve attestations for a repository.";
        Checks = "checks", "Checks", [Read, Write],
            "Checks on code.";
        CodeQuality = "code_quality", "Code quality", [Read, Write],
            "Access and manage code quality data for a repository.";
        CodeScanningAlerts = "security_events", "Code scanning alerts", [Read, Write],
            "View and manage code scanning alerts.";
        Codespaces = "codespaces", "Codespaces", [Read, Write],
            "Create, edit, delete and list Codespaces.";
        CodespacesLifecycleAdmin = "codespaces_lifecycle_admin", "Codespaces lifecycle admin", [Read, Write],
            "Manage the lifecycle of Codespaces, including starting and stopping.";
        CodespacesMetadata = "codespaces_metadata", "Codespaces metadata", [Read],
            "Access Codespaces metadata including the devcontainers and machine type.";
        CodespacesSecrets = "codespaces_secrets", "Codespaces secrets", [Write],
            "Restrict Codespaces user secrets modifications to specific repositories.";
        CommitStatuses = "statuses", "Commit statuses", [Read, Write],
            "Commit statuses.";
        Contents = "contents", "Contents", [Read, Write],
            "Repository contents, commits, branches, downloads, releases, and merges.";
        CopilotAgentSettings = "copilot_agent_settings", "Copilot agent settings", [Read, Write],
            "View and manage Copilot cloud agent settings for a repository.";
        CustomProperties = "repository_custom_properties", "Custom properties", [Read, Write],
            "Read and write repository custom properties values at the repository level, when allowed by the property.";
        DependabotAlerts = "vulnerability_alerts", "Dependabot alerts", [Read, Write],
            "Retrieve Dependabot alerts.";
        DependabotSecrets = "dependabot_secrets", "Dependabot secrets", [Read, Write],
            "Manage Dependabot repository secrets.";
        Deployments = "deployments", "Deployments", [Read, Write],
            "Deployments and deployment statuses.";
        Discussions = "discussions", "Discussions", [Read, Write],
            "Discussions and related comments and labels.";
        Environments = "environments", "Environments", [Read, Write],
            "Manage repository environments.";
        InstallationRepositories = "installation_repositories", "GitHub App installation repository access", [Write],
            "Manage which GitHub Apps can access a repository.";
        Issues = "issues", "Issues", [Read, Write],
            "Issues and related comments, assignees, labels, and milestones.";
        LicenseComplianceAlerts = "license_compliance_alerts", "License compliance alerts", [Read, Write],
            "Retrieve license compliance alerts.";
        MergeQueues = "merge_queues", "Merge queues", [Read, Write],
            "Manage a repository's merge queues.";
        Metadata = "metadata", "Metadata", [Read],
            "Search repositories, list collaborators, and access repository metadata.";
        Packages = "packages", "Packages", [Read, Write],
            "Packages published to the GitHub Package Platform.";
        Pages = "pages", "Pages", [Read, Write],
            "Retrieve Pages statuses, configuration, and builds, as well as create new builds.";
        Projects = "repository_projects", "Projects", [Read, Write, Admin],
            "Manage classic projects within a repository.";
        PullRequests = "pull_requests", "Pull requests", [Read, Write],
            "Pull requests and related comments, assignees, labels, milestones, and merges.";
        RepositoryCreation = "repository_creation", "Repository creation", [Write],
            "Create repositories.";
        RepositoryLicensePolicy = "repository_license_policy", "Repository license policy", [Read],
            "View the repository license policy and check package compliance.";
        RepositorySecurityAdvisories = "repository_advisories", "Repository security advisories", [Read, Write],
            "View and manage repository security advisories.";
        SecretScanningAlertDismissalRequests = "repo_secret_scanning_dismissal_requests", "Secret scanning alert dismissal requests", [Read, Write],
            "View and manage secret scanning alert dismissal requests.";
        SecretScanningAlerts = "secret_scanning_alerts", "Secret scanning alerts", [Read, Write],
            "View and manage secret scanning alerts.";
        SecretScanningPushProtectionBypassRequests = "secret_scanning_bypass_requests", "Secret scanning push protection bypass requests", [Read, Write],
            "Review and manage repository secret scanning push protection bypass requests.";
        Secrets = "secrets", "Secrets", [Read, Write],
            "Manage Actions repository secrets.";
        SingleFile = "single_file", "Single file", [Read, Write],
            "Manage just a single file.";
        Variables = "actions_variables", "Variables", [Read, Write],
            "Manage Actions repository variables.";
        Webhooks = "repository_hooks", "Webhooks", [Read, Write],
            "Manage the post-receive hooks for a repository.";
        Workflows = "workflows", "Workflows", [Write],
            "Update GitHub Action workflow files.";
    }
}

permission_enum! {
    /// Organization permissions permit access to organization related resources.
    pub enum OrganizationPermission {
        ApiInsights = "organization_api_insights", "API Insights", [Read],
            "View statistics on how the API is being used for an organization.";
        Administration = "organization_administration", "Administration", [Read, Write],
            "Manage access to an organization.";
        AgentSecrets = "organization_agent_secrets", "Agent secrets", [Read, Write],
            "Manage agent organization secrets.";
        AgentVariables = "organization_agent_variables", "Agent variables", [Read, Write],
            "Manage agent organization variables.";
        BlockingUsers = "organization_user_blocking", "Blocking users", [Read, Write],
            "View and manage users blocked by the organization.";
        Campaigns = "organization_campaigns", "Campaigns", [Read, Write],
            "Manage campaigns.";
        CopilotSpaces = "organization_copilot_spaces", "Copilot Spaces", [Read, Write],
            "View and manage Copilot Spaces for an organization.";
        CopilotAgentSettings = "organization_copilot_agent_settings", "Copilot agent settings", [Read, Write],
            "Manage Copilot cloud agent settings for an organization.";
        CopilotContentExclusion = "org_copilot_content_exclusion", "Copilot content exclusion", [Read, Write],
            "Manage Copilot content exclusion rules for the organization.";
        CustomOrganizationRoles = "organization_custom_org_roles", "Custom organization roles", [Read, Write],
            "Create, edit, delete and list custom organization roles. View system organization roles.";
        CustomProperties = "organization_custom_properties", "Custom properties", [Read, Write, Admin],
            "Read and write repository custom properties values and administer definitions at the organization level.";
        CustomPropertiesForOrganizations = "custom_properties_for_organizations", "Custom properties for organizations", [Read, Write],
            "Read and write organization custom properties values at the organization level, when allowed by the property.";
        CustomRepositoryRoles = "organization_custom_roles", "Custom repository roles", [Read, Write],
            "Create, edit, delete and list custom repository roles.";
        Events = "organization_events", "Events", [Read],
            "View events triggered by an activity in an organization.";
        ExternalCustomPropertiesForRepositories = "organization_external_properties_for_repos", "External custom properties for repositories", [Read, Write, Admin],
            "View and edit external custom properties for repositories, and manage the app installations that provide them, at the organization level.";
        CopilotBusiness = "organization_copilot_seat_management", "GitHub Copilot Business", [Read, Write],
            "Manage Copilot Business seats and settings.";
        HostedRunnerCustomImages = "organization_runner_custom_images", "Hosted runner custom images", [Read, Write],
            "View and manage hosted runner custom images available to an organization.";
        IssueFields = "issue_fields", "Issue Fields", [Read, Write],
            "Manage issue fields for an organization.";
        IssueTypes = "issue_types", "Issue Types", [Read, Write],
            "Manage issue types for an organization.";
        Members = "members", "Members", [Read, Write],
            "Organization members and teams.";
        Models = "organization_models", "Models", [Read],
            "Manage model access for an organization.";
        NetworkConfigurations = "organization_network_configurations", "Network configurations", [Read, Write],
            "View and manage hosted compute network configurations available to an organization.";
        CopilotMetrics = "organization_copilot_metrics", "Organization Copilot metrics", [Read],
            "View organization Copilot metrics.";
        AnnouncementBanners = "organization_announcement_banners", "Organization announcement banners", [Read, Write],
            "View and modify announcement banners for an organization.";
        SecretScanningBypassRequests = "organization_secret_scanning_bypass_requests", "Organization bypass requests for secret scanning", [Read, Write],
            "Review and manage secret scanning push protection bypass requests.";
        Codespaces = "organization_codespaces", "Organization codespaces", [Read, Write],
            "Manage Codespaces for an organization.";
        CodespacesSecrets = "organization_codespaces_secrets", "Organization codespaces secrets", [Read, Write],
            "Manage Codespaces Secrets for an organization.";
        CodespacesSettings = "organization_codespaces_settings", "Organization codespaces settings", [Read, Write],
            "Manage Codespaces settings for an organization.";
        Credentials = "organization_credentials", "Organization credentials", [Read, Write],
            "View and manage organization credentials.";
        DependabotSecrets = "organization_dependabot_secrets", "Organization dependabot secrets", [Read, Write],
            "Manage Dependabot organization secrets.";
        DependabotDismissalRequests = "organization_dependabot_dismissal_requests", "Organization dismissal requests for Dependabot", [Read, Write],
            "Review and manage Dependabot alert dismissal requests.";
        CodeScanningDismissalRequests = "organization_code_scanning_dismissal_requests", "Organization dismissal requests for code scanning", [Read, Write],
            "Review and manage code scanning alert dismissal requests.";
        InnersourceVulnerabilities = "organization_innersource_vulnerabilities", "Organization innersource vulnerabilities", [Read, Write],
            "View and manage innersource vulnerabilities across the organization.";
        PrivateRegistries = "organization_private_registries", "Organization private registries", [Read, Write],
            "Manage private registries for an organization.";
        PersonalAccessTokenRequests = "organization_personal_access_token_requests", "Personal access token requests", [Read, Write],
            "Manage personal access token requests from organization members.";
        PersonalAccessTokens = "organization_personal_access_tokens", "Personal access tokens", [Read, Write],
            "View and revoke personal access tokens that have been granted access to an organization.";
        Plan = "organization_plan", "Plan", [Read],
            "View an organization's plan.";
        Projects = "organization_projects", "Projects", [Read, Write, Admin],
            "Manage projects for an organization.";
        SecretScanningAlertDismissalRequests = "secret_scanning_dismissal_requests", "Secret scanning alert dismissal requests", [Read, Write],
            "Review and manage secret scanning alert dismissal requests.";
        Secrets = "organization_secrets", "Secrets", [Read, Write],
            "Manage Actions organization secrets.";
        SelfHostedRunners = "organization_self_hosted_runners", "Self-hosted runners", [Read, Write],
            "View and manage Actions self-hosted runners available to an organization.";
        Variables = "organization_actions_variables", "Variables", [Read, Write],
            "Manage Actions organization variables.";
        Webhooks = "organization_hooks", "Webhooks", [Read, Write],
            "Manage the post-receive hooks for an organization.";
        // Documented, but not on the settings page as reviewed.
        Packages = "organization_packages", "Organization packages", [Read, Write],
            "Organization packages published to GitHub Packages. Not on the settings page; from the REST API description.";
        CopilotRequests = "organization_copilot_requests", "Copilot requests", [Read],
            "Make Copilot requests billed to the organization (public preview). Not on the settings page; from the REST API description.";
        KnowledgeBases = "organization_knowledge_bases", "Knowledge bases", [Read, Write],
            "Organization knowledge bases. Not on the settings page; from the fine-grained token docs.";
        TeamDiscussions = "team_discussions", "Team discussions", [Read, Write],
            "Team discussions. Not on the settings page; from the fine-grained token docs.";
    }
}

permission_enum! {
    /// Account permissions are granted on an individual user basis as part of
    /// the user authorization flow. They apply to user access tokens, not to
    /// installation tokens, so [`Permissions`] does not accept them.
    pub enum AccountPermission {
        BlockAnotherUser = "blocking", "Block another user", [Read, Write],
            "View and manage users blocked by the user.";
        CodespacesUserSecrets = "codespaces_user_secrets", "Codespaces user secrets", [Read, Write],
            "Manage Codespaces user secrets.";
        CopilotChat = "copilot_messages", "Copilot Chat", [Read],
            "This application will receive your GitHub ID, your GitHub Copilot Chat session messages (not including messages sent to another application), and timestamps of provided GitHub Copilot Chat session messages. This permission must be enabled for Copilot Extensions.";
        CopilotEditorContext = "copilot_editor_context", "Copilot Editor Context", [Read],
            "This application will receive bits of Editor Context (e.g. currently opened file) whenever you send it a message through Copilot Chat.";
        EmailAddresses = "emails", "Email addresses", [Read, Write],
            "Manage a user's email addresses. The REST API description's request schema (and clients generated from it) calls this `email_addresses`; the settings form, GitHub's permission docs and webhook payloads use `emails`.";
        Events = "user_events", "Events", [Read],
            "View events triggered by a user's activity.";
        Followers = "followers", "Followers", [Read, Write],
            "A user's followers.";
        GpgKeys = "gpg_keys", "GPG keys", [Read, Write],
            "View and manage a user's GPG keys.";
        Gists = "gists", "Gists", [Write],
            "Create and modify a user's gists and comments.";
        GitSshKeys = "keys", "Git SSH keys", [Read, Write],
            "Git SSH keys. The REST API description's request schema (and clients generated from it) calls this `git_ssh_keys`; the settings form, GitHub's permission docs and the `octokit/webhooks` app schema use `keys`.";
        InteractionLimits = "interaction_limits", "Interaction limits", [Read, Write],
            "Interaction limits on repositories.";
        IssueFields = "user_issue_fields", "Issue Fields", [Read, Write],
            "Manage issue fields for a user.";
        IssueTypes = "user_issue_types", "Issue Types", [Read, Write],
            "Manage issue types for a user.";
        Models = "user_models", "Models", [Read],
            "Allows access to GitHub Models.";
        Plan = "plan", "Plan", [Read],
            "View a user's plan.";
        PluginGatewayConnections = "plugin_gateway_connections", "Plugin gateway connections", [Read, Write],
            "Manage plugin gateway connections, their MCP configuration, and MCP invocation through them.";
        Profile = "profile", "Profile", [Write],
            "Manage a user's profile settings.";
        SshSigningKeys = "git_signing_ssh_public_keys", "SSH signing keys", [Read, Write],
            "View and manage a user's SSH signing keys.";
        Starring = "starring", "Starring", [Read, Write],
            "List and manage repositories a user is starring.";
        Watching = "watching", "Watching", [Read, Write],
            "List and change repositories a user is subscribed to.";
        // Documented, but not on the settings page as reviewed.
        CopilotRequests = "copilot_requests", "Copilot requests", [Write],
            "Make Copilot requests for the user, counted against their premium request allowance. Not on the settings page; from the fine-grained token docs.";
        KnowledgeBases = "knowledge_bases", "Knowledge bases", [Read, Write],
            "The user's knowledge bases. Not on the settings page; from the fine-grained token docs.";
        PrivateRepositoryInvitations = "private_repository_invitations", "Private repository invitations", [Read],
            "View the user's invitations to private repositories. Not on the settings page; from the fine-grained token docs.";
    }
}

permission_enum! {
    /// Enterprise permissions permit access to enterprise account resources
    /// (preview). Only enterprise installations can be granted them.
    pub enum EnterprisePermission {
        CopilotUsageRecords = "enterprise_copilot_usage", "Copilot usage records", [Read],
            "View enterprise Copilot API usage records.";
        CustomEnterpriseRoles = "enterprise_custom_enterprise_roles", "Custom enterprise roles", [Read, Write],
            "Manage enterprise custom roles and assignments.";
        CustomProperties = "enterprise_custom_properties", "Custom properties", [Read, Write],
            "View repository custom properties and administer definitions at the enterprise level.";
        AiControls = "enterprise_ai_controls", "Enterprise AI controls", [Read, Write],
            "Manage enterprise-wide AI controls configuration.";
        CopilotMetrics = "enterprise_copilot_metrics", "Enterprise Copilot metrics", [Read],
            "View enterprise Copilot metrics.";
        Scim = "enterprise_scim", "Enterprise SCIM", [Read, Write],
            "View and manage enterprise SCIM configuration in an Enterprise Managed User enterprise.";
        Billing = "enterprise_billing", "Enterprise billing", [Read, Write],
            "Access usage, budgets, cost centers, and payment information for the enterprise.";
        Credentials = "enterprise_credentials", "Enterprise credentials", [Read, Write],
            "View and manage enterprise credentials.";
        CustomOrganizationRoles = "enterprise_custom_org_roles", "Enterprise custom organization roles", [Read, Write],
            "Create, edit, delete and list custom organization roles at the enterprise level. View system organization roles.";
        CustomPropertiesForOrganizations = "enterprise_custom_properties_for_organizations", "Enterprise custom properties for organizations", [Read, Write, Admin],
            "View organization custom properties and administer definitions at the enterprise level.";
        InnersourceVulnerabilities = "enterprise_innersource_vulnerabilities", "Enterprise innersource vulnerabilities", [Read, Write],
            "View and manage innersource vulnerabilities across the enterprise.";
        LicensePolicy = "enterprise_license_policy", "Enterprise license policy", [Read, Write],
            "View and manage the enterprise license policy.";
        OrganizationInstallationRepositories = "enterprise_organization_installation_repositories", "Enterprise organization installation repositories", [Read, Write],
            "Manage repository access of GitHub Apps on Enterprise-owned organizations. Requesting this permission will prevent it from being installed on other enterprises.";
        OrganizationInstallations = "enterprise_organization_installations", "Enterprise organization installations", [Read, Write],
            "Manage installation of GitHub Apps on Enterprise-owned organizations. Requesting this permission will prevent it from being installed on other enterprises.";
        Organizations = "enterprise_organizations", "Enterprise organizations", [Write],
            "Create and remove enterprise organizations.";
        People = "enterprise_people", "Enterprise people", [Read, Write],
            "Manage user access to the enterprise.";
        SingleSignOn = "enterprise_sso", "Enterprise single sign-on", [Read, Write],
            "View and manage enterprise single sign-on configuration.";
        Teams = "enterprise_teams", "Enterprise teams", [Read, Write],
            "Create, edit, remove and view enterprise teams.";
        // Documented, but not on the settings page as reviewed.
        Administration = "enterprise_administration", "Enterprise administration", [Read, Write],
            "Enterprise administration. Not on the settings page; from the GHEC REST API description.";
        CopilotSettings = "enterprise_copilot_settings", "Copilot settings", [Write],
            "Manage the enterprise Copilot subscription's teams. Not on the settings page; access levels endpoint-derived.";
        Licensing = "enterprise_licensing", "Enterprise Licensing", [Read, Write],
            "View and manage Visual Studio subscriptions. Not on the settings page; access levels endpoint-derived.";
    }
}

/// A permission an installation token can request: repository, organization
/// or enterprise. Account permissions are not requestable (see
/// [`AccountPermission`]).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[non_exhaustive]
pub enum Permission {
    /// A repository permission.
    Repository(RepositoryPermission),

    /// An organization permission.
    Organization(OrganizationPermission),

    /// An enterprise permission.
    Enterprise(EnterprisePermission),
}

impl Permission {
    /// GitHub's JSON key.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Repository(p) => p.as_str(),
            Self::Organization(p) => p.as_str(),
            Self::Enterprise(p) => p.as_str(),
        }
    }

    /// The name on GitHub's settings page.
    pub const fn title(self) -> &'static str {
        match self {
            Self::Repository(p) => p.title(),
            Self::Organization(p) => p.title(),
            Self::Enterprise(p) => p.title(),
        }
    }

    /// The help text on GitHub's settings page.
    pub const fn description(self) -> &'static str {
        match self {
            Self::Repository(p) => p.description(),
            Self::Organization(p) => p.description(),
            Self::Enterprise(p) => p.description(),
        }
    }

    /// The access levels GitHub accepts for this permission.
    pub const fn access_levels(self) -> &'static [Access] {
        match self {
            Self::Repository(p) => p.access_levels(),
            Self::Organization(p) => p.access_levels(),
            Self::Enterprise(p) => p.access_levels(),
        }
    }

    /// Whether GitHub accepts `access` for this permission.
    pub const fn accepts(self, access: Access) -> bool {
        match self {
            Self::Repository(p) => p.accepts(access),
            Self::Organization(p) => p.accepts(access),
            Self::Enterprise(p) => p.accepts(access),
        }
    }
}

impl std::fmt::Display for Permission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Serializes as GitHub's JSON key.
impl serde::Serialize for Permission {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl From<RepositoryPermission> for Permission {
    fn from(p: RepositoryPermission) -> Self {
        Self::Repository(p)
    }
}

impl From<OrganizationPermission> for Permission {
    fn from(p: OrganizationPermission) -> Self {
        Self::Organization(p)
    }
}

impl From<EnterprisePermission> for Permission {
    fn from(p: EnterprisePermission) -> Self {
        Self::Enterprise(p)
    }
}

/// A validated, nonempty set of permissions for one installation token: at
/// most one access level per permission, and only levels GitHub accepts.
/// Serializes as GitHub's `permissions` object.
///
/// Always explicit: this crate never creates a token that silently inherits
/// every permission the installation was granted.
///
/// ```
/// use octoauth::{Access, OrganizationPermission, Permissions, RepositoryPermission};
///
/// // One kind:
/// let read = Permissions::new([
///     (RepositoryPermission::Contents, Access::Read),
///     (RepositoryPermission::Metadata, Access::Read),
/// ])?;
///
/// // Mixed kinds:
/// let invite = Permissions::builder()
///     .grant(OrganizationPermission::Members, Access::Write)
///     .grant(RepositoryPermission::Metadata, Access::Read)
///     .build()?;
///
/// // Rejected: workflows is write-only.
/// assert!(Permissions::new([(RepositoryPermission::Workflows, Access::Read)]).is_err());
/// # Ok::<(), octoauth::ConfigError>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
/// Keyed by JSON key, so iteration and serialization are alphabetical.
pub struct Permissions(BTreeMap<&'static str, (Permission, Access)>);

impl Permissions {
    /// Validate permissions of one kind (or already converted to [`Permission`]).
    /// See [`PermissionsBuilder::build`] for the errors.
    pub fn new<P: Into<Permission>>(
        entries: impl IntoIterator<Item = (P, Access)>,
    ) -> Result<Self, ConfigError> {
        entries
            .into_iter()
            .fold(Self::builder(), |builder, (permission, access)| {
                builder.grant(permission, access)
            })
            .build()
    }

    /// Collect permissions of any kind, validated by
    /// [`build`](PermissionsBuilder::build).
    pub fn builder() -> PermissionsBuilder {
        PermissionsBuilder(Vec::new())
    }

    /// Requested permissions, ordered by JSON key.
    pub fn iter(&self) -> impl Iterator<Item = (Permission, Access)> + '_ {
        self.0.values().copied()
    }

    /// The requested access for `permission`, if any.
    pub fn get(&self, permission: impl Into<Permission>) -> Option<Access> {
        self.0.get(permission.into().as_str()).map(|(_, a)| *a)
    }

    /// Whether every entry is `read`.
    pub fn is_read_only(&self) -> bool {
        self.0.values().all(|(_, a)| *a == Access::Read)
    }
}

/// Serializes as GitHub's `permissions` object, keys in alphabetical order:
/// `{"members":"write","metadata":"read"}`.
impl serde::Serialize for Permissions {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.0.iter().map(|(key, (_, access))| (key, access)))
    }
}

/// Builder for [`Permissions`], from [`Permissions::builder`].
#[derive(Clone, Debug, Default)]
#[must_use]
pub struct PermissionsBuilder(Vec<(Permission, Access)>);

impl PermissionsBuilder {
    /// Request `access` to `permission`.
    pub fn grant(mut self, permission: impl Into<Permission>, access: Access) -> Self {
        self.0.push((permission.into(), access));
        self
    }

    /// Validate. Errors ([`ConfigError::field`], [`ConfigError::what`]):
    ///
    /// - `("permissions", "empty")`: nothing granted;
    /// - `(<key>, "duplicate")`: the permission with JSON key `<key>` was
    ///   granted twice;
    /// - `(<key>, "access")`: GitHub does not accept that access level for it,
    ///   e.g. `read` for `workflows`.
    pub fn build(self) -> Result<Permissions, ConfigError> {
        if self.0.is_empty() {
            return Err(ConfigError::new("permissions", "empty"));
        }
        let mut map = BTreeMap::new();
        for (permission, access) in self.0 {
            if !permission.accepts(access) {
                return Err(ConfigError::new(permission.as_str(), "access"));
            }
            if map
                .insert(permission.as_str(), (permission, access))
                .is_some()
            {
                return Err(ConfigError::new(permission.as_str(), "duplicate"));
            }
        }
        Ok(Permissions(map))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn keys() -> Vec<&'static str> {
        RepositoryPermission::ALL
            .iter()
            .map(|p| p.as_str())
            .chain(OrganizationPermission::ALL.iter().map(|p| p.as_str()))
            .chain(AccountPermission::ALL.iter().map(|p| p.as_str()))
            .chain(EnterprisePermission::ALL.iter().map(|p| p.as_str()))
            .collect()
    }

    #[test]
    fn keys_are_unique_snake_case() {
        let keys = keys();
        assert_eq!(keys.len(), keys.iter().collect::<BTreeSet<_>>().len());
        for key in keys {
            assert!(
                key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{key}"
            );
        }
    }

    #[test]
    fn every_permission_is_described_and_has_levels() {
        macro_rules! check {
            ($($ty:ty),+) => {$(
                for p in <$ty>::ALL {
                    assert!(!p.title().is_empty() && p.description().ends_with('.'), "{p}");
                    let levels = p.access_levels();
                    assert!(!levels.is_empty(), "{p}");
                    assert!(levels.windows(2).all(|w| w[0] < w[1]), "{p}");
                    for access in [Access::Read, Access::Write, Access::Admin] {
                        assert_eq!(p.accepts(access), levels.contains(&access), "{p}");
                    }
                }
            )+};
        }
        check!(
            RepositoryPermission,
            OrganizationPermission,
            AccountPermission,
            EnterprisePermission
        );
    }

    #[test]
    fn documented_levels() {
        use Access::*;
        assert_eq!(RepositoryPermission::Metadata.access_levels(), &[Read]);
        assert_eq!(RepositoryPermission::Workflows.access_levels(), &[Write]);
        assert_eq!(
            RepositoryPermission::Projects.access_levels(),
            &[Read, Write, Admin]
        );
        assert_eq!(
            OrganizationPermission::Members.access_levels(),
            &[Read, Write]
        );
        assert_eq!(AccountPermission::Profile.access_levels(), &[Write]);
        assert_eq!(
            RepositoryPermission::CodeScanningAlerts.as_str(),
            "security_events"
        );
        assert_eq!(
            RepositoryPermission::DependabotAlerts.as_str(),
            "vulnerability_alerts"
        );
        assert_eq!(
            RepositoryPermission::Variables.as_str(),
            "actions_variables"
        );
        // Keys only the settings form names, and levels it corrected.
        assert_eq!(AccountPermission::IssueFields.as_str(), "user_issue_fields");
        assert_eq!(AccountPermission::EmailAddresses.as_str(), "emails");
        assert_eq!(AccountPermission::GitSshKeys.as_str(), "keys");
        assert_eq!(
            EnterprisePermission::SingleSignOn.as_str(),
            "enterprise_sso"
        );
        assert_eq!(
            RepositoryPermission::CopilotAgentSettings.access_levels(),
            &[Read, Write]
        );
    }

    #[test]
    fn canonical_json() {
        let permissions = Permissions::builder()
            .grant(RepositoryPermission::Metadata, Access::Read)
            .grant(OrganizationPermission::Members, Access::Write)
            .grant(RepositoryPermission::Administration, Access::Write)
            .build()
            .unwrap();
        assert_eq!(
            serde_json::to_string(&permissions).unwrap(),
            r#"{"administration":"write","members":"write","metadata":"read"}"#
        );
        assert!(!permissions.is_read_only());
        assert_eq!(
            permissions.get(OrganizationPermission::Members),
            Some(Access::Write)
        );
        assert_eq!(permissions.get(RepositoryPermission::Contents), None);
        assert_eq!(permissions.iter().count(), 3);
    }

    #[test]
    fn rejects_invalid_requests() {
        let error = |result: Result<Permissions, ConfigError>| {
            let e = result.unwrap_err();
            (e.field(), e.what())
        };
        assert_eq!(
            error(Permissions::new::<Permission>([])),
            ("permissions", "empty")
        );
        assert_eq!(
            error(Permissions::new([
                (OrganizationPermission::Members, Access::Read),
                (OrganizationPermission::Members, Access::Write),
            ])),
            ("members", "duplicate")
        );
        for (permission, access) in [
            (
                Permission::from(RepositoryPermission::Workflows),
                Access::Read,
            ),
            (RepositoryPermission::Metadata.into(), Access::Write),
            (RepositoryPermission::Contents.into(), Access::Admin),
            (EnterprisePermission::Organizations.into(), Access::Read),
        ] {
            assert_eq!(
                error(Permissions::new([(permission, access)])),
                (permission.as_str(), "access")
            );
        }
    }

    #[test]
    fn read_only() {
        assert!(
            Permissions::new([(OrganizationPermission::Members, Access::Read)])
                .unwrap()
                .is_read_only()
        );
    }

    #[test]
    fn values_serialize_as_github_strings() {
        assert_eq!(serde_json::to_string(&Access::Admin).unwrap(), r#""admin""#);
        assert_eq!(
            serde_json::to_string(&RepositoryPermission::CodeScanningAlerts).unwrap(),
            r#""security_events""#
        );
        assert_eq!(
            serde_json::to_string(&Permission::from(OrganizationPermission::Members)).unwrap(),
            r#""members""#
        );
        assert_eq!(
            serde_json::to_string(&AccountPermission::EmailAddresses).unwrap(),
            r#""emails""#
        );
    }

    #[test]
    fn display_and_metadata() {
        let p = Permission::from(RepositoryPermission::Metadata);
        assert_eq!(p.to_string(), "metadata");
        assert_eq!(p.title(), "Metadata");
        assert_eq!(
            p.description(),
            "Search repositories, list collaborators, and access repository metadata."
        );
    }
}
