//! Atomic installation/settings projection plus all retained audit facts.
use super::{Error, Result, installations};
use crate::{account::AccountInstallationRef, installation_audit::InstallationChange};

pub fn encode(owner: AccountInstallationRef, change: &InstallationChange) -> Result<String> {
    let events = change
        .events(owner)
        .map_err(|e| Error::ProjectionInvariant(e.into()))?;
    super::integer::validate(&events)?;
    let events: Vec<_> = events
        .iter()
        .map(|event| {
            let mut value = serde_json::to_value(event).expect("typed event");
            value["occurred_at"] = serde_json::Value::String(event.occurred_at.to_rfc3339());
            value
        })
        .collect();
    let installation_content = change
        .installation
        .as_ref()
        .map(installations::encode_app)
        .transpose()?;
    let settings_content = change
        .settings
        .as_ref()
        .map(installations::encode_settings)
        .transpose()?;
    Ok(serde_json::json!({"installation":change.installation,"settings":change.settings,
        "installation_content":installation_content,"settings_content":settings_content,
        "installed_at":change.installation.as_ref().map(|v|v.account.installed_at.to_rfc3339()),
        "uninstalled_at":change.installation.as_ref().and_then(|v|v.account.uninstalled_at.map(|at|at.to_rfc3339())),
        "suspended_at":change.installation.as_ref().and_then(|v|v.suspended_at.map(|at|at.to_rfc3339())),
        "settings_changed_at":change.settings.as_ref().map(|v|v.changed_at.to_rfc3339()),
        "selected_repos":change.installation.as_ref().map(|v|installations::encode_selected_repos(&v.account.selected_repos)),
        "permissions":change.installation.as_ref().map(|v|serde_json::to_string(&v.permissions).expect("typed permissions")),"events":events}).to_string())
}
const INSTALLATION: &str = r#"INSERT INTO installations(app,installation_id,account_id,account_login,account_type,installed_at,uninstalled_at,selected_repos,suspended_at,permissions_json,installed_by_user_id,installed_by_login,projection_revision,projection_content)
SELECT json_extract(?1,'$.installation.app'),json_extract(?1,'$.installation.account.installation_id'),json_extract(?1,'$.installation.account.account_id'),json_extract(?1,'$.installation.account.account_login'),json_extract(?1,'$.installation.account.account_type'),json_extract(?1,'$.installed_at'),json_extract(?1,'$.uninstalled_at'),json_extract(?1,'$.selected_repos'),json_extract(?1,'$.suspended_at'),json_extract(?1,'$.permissions'),json_extract(?1,'$.installation.installed_by_user_id'),json_extract(?1,'$.installation.installed_by_login'),json_extract(?1,'$.installation.revision'),json_extract(?1,'$.installation_content')
WHERE json_type(?1,'$.installation')='object'
ON CONFLICT(app,installation_id) DO UPDATE SET account_login=excluded.account_login,uninstalled_at=excluded.uninstalled_at,selected_repos=excluded.selected_repos,suspended_at=excluded.suspended_at,permissions_json=excluded.permissions_json,installed_by_user_id=excluded.installed_by_user_id,installed_by_login=excluded.installed_by_login,projection_revision=excluded.projection_revision,projection_content=excluded.projection_content
WHERE installations.account_id=excluded.account_id AND installations.account_type=excluded.account_type AND installations.installed_at=excluded.installed_at AND excluded.projection_revision>installations.projection_revision AND (installations.uninstalled_at IS NULL OR installations.uninstalled_at IS excluded.uninstalled_at) AND (installations.installed_by_user_id IS NULL OR excluded.installed_by_user_id IS NOT NULL)"#;
const VERIFY_INSTALLATION: &str = r#"INSERT INTO projection_assertions(invariant)
SELECT json_type(?1,'$.installation')!='object' OR EXISTS(SELECT 1 FROM installations WHERE app=json_extract(?1,'$.installation.app') AND installation_id=json_extract(?1,'$.installation.account.installation_id') AND account_id=json_extract(?1,'$.installation.account.account_id') AND account_type=json_extract(?1,'$.installation.account.account_type') AND installed_at=json_extract(?1,'$.installed_at') AND projection_revision>=json_extract(?1,'$.installation.revision') AND (projection_revision>json_extract(?1,'$.installation.revision') OR projection_content=json_extract(?1,'$.installation_content')))"#;
const SETTINGS: &str = r#"INSERT INTO account_settings(account_id,repo_admin_delegation,changed_by,changed_at,projection_revision,projection_content)
SELECT json_extract(?1,'$.settings.account_id'),json_extract(?1,'$.settings.repo_admin_delegation'),json_extract(?1,'$.settings.changed_by'),json_extract(?1,'$.settings_changed_at'),json_extract(?1,'$.settings.revision'),json_extract(?1,'$.settings_content') WHERE json_type(?1,'$.settings')='object'
ON CONFLICT(account_id) DO UPDATE SET repo_admin_delegation=excluded.repo_admin_delegation,changed_by=excluded.changed_by,changed_at=excluded.changed_at,projection_revision=excluded.projection_revision,projection_content=excluded.projection_content WHERE excluded.projection_revision>account_settings.projection_revision"#;
const VERIFY_SETTINGS: &str = r#"INSERT INTO projection_assertions(invariant)
SELECT json_type(?1,'$.settings')!='object' OR EXISTS(SELECT 1 FROM account_settings WHERE account_id=json_extract(?1,'$.settings.account_id') AND projection_revision>=json_extract(?1,'$.settings.revision') AND (projection_revision>json_extract(?1,'$.settings.revision') OR projection_content=json_extract(?1,'$.settings_content')))"#;
pub fn statements() -> Vec<String> {
    vec![
        INSTALLATION.into(),
        VERIFY_INSTALLATION.into(),
        SETTINGS.into(),
        VERIFY_SETTINGS.into(),
        super::audit_write::retained_events(),
        "DELETE FROM projection_assertions WHERE ?1 IS NOT NULL".into(),
    ]
}
