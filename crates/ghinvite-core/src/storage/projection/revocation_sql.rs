//! Independent immutable revocation receipt projection; progress remains live authority data.
use crate::{
    revocation::RevocationProjection,
    storage::{Error, Result},
};
use serde_json::json;
use sha2::{Digest, Sha256};
pub fn encode(value: &RevocationProjection) -> Result<String> {
    crate::storage::integer::validate(value)?;
    if value.revision == 0
        || value.command.admin.account_id == 0
        || value.command.admin.user_id == 0
        || (!value.command.cascade.remove_granted_access
            && value.accepted.addressed_access_grants != 0)
    {
        return Err(Error::ProjectionInvariant(
            "invalid revocation projection".into(),
        ));
    }
    let event_id = format!(
        "invitation_link.revoke_cascade_requested/{}/{}",
        value.command.link_id,
        String::from(value.command.operation_id.clone())
    );
    let digest = Sha256::digest(format!("ghinvite/projection/audit/{event_id}"));
    let id = ulid::Ulid::from_bytes(digest[..16].try_into().unwrap()).to_string();
    let mut metadata = json!({"options":value.command.cascade,"addressed_requests":value.accepted.addressed_requests,"addressed_deliveries":value.accepted.addressed_deliveries});
    if value.accepted.addressed_access_grants > 0 {
        metadata["addressed_access_grants"] = json!(value.accepted.addressed_access_grants);
    }
    serde_json::to_string(&json!({"receipt":value,"metadata":metadata,"id":id,"event_id":event_id,"audit":value.command.cascade.any()})).map_err(|_|Error::ProjectionInvariant("invalid revocation projection".into()))
}
pub const STATEMENTS: &[&str] = &[
    r#"INSERT INTO projection_assertions(dependency,invariant) SELECT
      EXISTS(SELECT 1 FROM invitation_links WHERE id=json_extract(?1,'$.receipt.command.link_id')) AND EXISTS(SELECT 1 FROM users WHERE user_id=json_extract(?1,'$.receipt.command.admin.user_id')),
      NOT EXISTS(SELECT 1 FROM invitation_links WHERE id=json_extract(?1,'$.receipt.command.link_id') AND
        (account_id IS NOT json_extract(?1,'$.receipt.command.admin.account_id') OR revoked_at IS NOT json_extract(?1,'$.receipt.at') OR revoked_by IS NOT json_extract(?1,'$.receipt.command.admin.user_id') OR projection_revision < json_extract(?1,'$.receipt.revision') OR
         (revoke_cascade_json!='{}' AND revoke_cascade_json IS NOT json_extract(?1,'$.receipt'))))"#,
    r#"INSERT INTO projection_assertions(invariant) SELECT NOT EXISTS(SELECT 1 FROM audit_events WHERE (id=json_extract(?1,'$.id') OR projection_event_id=json_extract(?1,'$.event_id')) AND (projection_content IS NOT ?1 OR id IS NOT json_extract(?1,'$.id') OR projection_event_id IS NOT json_extract(?1,'$.event_id') OR account_id IS NOT json_extract(?1,'$.receipt.command.admin.account_id') OR occurred_at IS NOT json_extract(?1,'$.receipt.at') OR event_type IS NOT 'invitation_link.revoke_cascade_requested' OR actor_kind IS NOT 'user' OR actor_id IS NOT json_extract(?1,'$.receipt.command.admin.user_id') OR target_kind IS NOT 'invitation_link' OR target_id IS NOT json_extract(?1,'$.receipt.command.link_id') OR metadata IS NOT json_extract(?1,'$.metadata') OR evaluated_at IS NOT json_extract(?1,'$.receipt.at')))"#,
    r#"UPDATE invitation_links SET revoke_cascade_json=json_extract(?1,'$.receipt') WHERE id=json_extract(?1,'$.receipt.command.link_id') AND revoke_cascade_json='{}'"#,
    r#"INSERT INTO audit_events(id,account_id,occurred_at,event_type,actor_kind,actor_id,target_kind,target_id,metadata,projection_event_id,projection_content,evaluated_at)
      SELECT json_extract(?1,'$.id'),json_extract(?1,'$.receipt.command.admin.account_id'),json_extract(?1,'$.receipt.at'),'invitation_link.revoke_cascade_requested','user',json_extract(?1,'$.receipt.command.admin.user_id'),'invitation_link',json_extract(?1,'$.receipt.command.link_id'),json_extract(?1,'$.metadata'),json_extract(?1,'$.event_id'),?1,json_extract(?1,'$.receipt.at')
      WHERE json_extract(?1,'$.audit')=1 ON CONFLICT(id) DO NOTHING"#,
    "DELETE FROM projection_assertions WHERE ?1 IS NOT NULL",
];
pub const PREVIEW: &str = r#"SELECT
 (SELECT count(*) FROM invitation_requests r WHERE r.invitation_link_id=l.id AND r.state='pending') pending_requests,
 (SELECT count(*) FROM deliveries d JOIN invitation_requests r ON r.id=d.request_id WHERE r.invitation_link_id=l.id AND d.create_outcome='created' AND d.tracked_foreign=0 AND (d.settlement IS NULL OR d.settlement='sent')) pending_invitations
 FROM invitation_links l WHERE l.id=?1 AND l.account_id=?2"#;
