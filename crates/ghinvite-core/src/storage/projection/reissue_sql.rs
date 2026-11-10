//! Atomic immutable reissue proof, current epoch and transition audit.
use super::{RequestProjectionEnvelope, request_sql};
use crate::{
    reissue_projection::RequestReissueProjection,
    storage::{Error, Result},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
pub fn encode(proof: &RequestReissueProjection) -> Result<String> {
    let invalid = || Error::ProjectionInvariant("invalid reissue projection".into());
    proof.validate().map_err(|_| invalid())?;
    crate::storage::integer::validate(proof)?;
    let original = RequestProjectionEnvelope {
        approval: None,
        request: proof.original_approval.clone(),
        events: proof.approval_events.clone(),
    };
    let next = RequestProjectionEnvelope {
        approval: None,
        request: proof.after.clone(),
        events: proof.approval_events.clone(),
    };
    let original: Value =
        serde_json::from_str(&request_sql::encode_single(&original)?).map_err(|_| invalid())?;
    let next: Value =
        serde_json::from_str(&request_sql::encode_single(&next)?).map_err(|_| invalid())?;
    let event_id = format!(
        "delivery.reissued/{}/{}/{}",
        proof.command.request_id,
        proof.command.target.key(),
        String::from(proof.command.operation_id.clone())
    );
    let digest = Sha256::digest(format!("ghinvite/projection/audit/{event_id}"));
    let id = ulid::Ulid::from_bytes(digest[..16].try_into().unwrap()).to_string();
    let (old, new) = proof.delivery.generations();
    let metadata =
        json!({"target":proof.command.target,"from_generation":old,"delivery_generation":new});
    serde_json::to_string(&json!({"proof":proof,"original":original,"next":next,"id":id,"event_id":event_id,"metadata":metadata})).map_err(|_|invalid())
}
pub fn statements() -> Vec<String> {
    let mut result = Vec::new();
    result.extend(
        request_sql::STATEMENTS
            .iter()
            .map(|sql| sql.replace("?1", "json_extract(?1,'$.original')")),
    );
    result.extend(PROOF.iter().map(|s| (*s).to_owned()));
    result.extend(
        request_sql::STATEMENTS
            .iter()
            .map(|sql| sql.replace("?1", "json_extract(?1,'$.next')")),
    );
    result.extend(AUDIT.iter().map(|s| (*s).to_owned()));
    result
}
const PROOF: &[&str] = &[
    r#"INSERT INTO projection_assertions(invariant) SELECT NOT EXISTS(
 SELECT 1 FROM request_reissues WHERE request_id=json_extract(?1,'$.proof.command.request_id')
 AND (operation_id=json_extract(?1,'$.proof.command.operation_id') OR after_revision=json_extract(?1,'$.proof.after.revision'))
 AND content IS NOT json_extract(?1,'$.proof'))"#,
    r#"INSERT INTO request_reissues(request_id,operation_id,before_revision,after_revision,before_content,after_content,content)
 VALUES(json_extract(?1,'$.proof.command.request_id'),json_extract(?1,'$.proof.command.operation_id'),json_extract(?1,'$.proof.before.revision'),json_extract(?1,'$.proof.after.revision'),json_extract(?1,'$.proof.before'),json_extract(?1,'$.proof.after'),json_extract(?1,'$.proof')) ON CONFLICT DO NOTHING"#,
];
const AUDIT: &[&str] = &[
    r#"INSERT INTO projection_assertions(dependency) SELECT EXISTS(SELECT 1 FROM users WHERE user_id=json_extract(?1,'$.proof.command.admin.user_id'))"#,
    r#"INSERT INTO projection_assertions(invariant) SELECT NOT EXISTS(SELECT 1 FROM audit_events
 WHERE (id=json_extract(?1,'$.id') OR projection_event_id=json_extract(?1,'$.event_id'))
 AND (projection_content IS NOT json_extract(?1,'$.proof') OR metadata IS NOT json_extract(?1,'$.metadata')
 OR account_id IS NOT json_extract(?1,'$.proof.command.admin.account_id') OR actor_id IS NOT json_extract(?1,'$.proof.command.admin.user_id')
 OR event_type IS NOT 'delivery.reissued' OR actor_kind IS NOT 'user' OR target_kind IS NOT 'invitation_request'
 OR target_id IS NOT json_extract(?1,'$.proof.command.request_id') OR occurred_at IS NOT json_extract(?1,'$.proof.at')
 OR evaluated_at IS NOT json_extract(?1,'$.proof.at')))"#,
    r#"INSERT INTO audit_events(id,account_id,occurred_at,event_type,actor_kind,actor_id,target_kind,target_id,metadata,projection_event_id,projection_content,evaluated_at)
 SELECT json_extract(?1,'$.id'),json_extract(?1,'$.proof.command.admin.account_id'),json_extract(?1,'$.proof.at'),'delivery.reissued','user',json_extract(?1,'$.proof.command.admin.user_id'),'invitation_request',json_extract(?1,'$.proof.command.request_id'),json_extract(?1,'$.metadata'),json_extract(?1,'$.event_id'),json_extract(?1,'$.proof'),json_extract(?1,'$.proof.at')
 WHERE true ON CONFLICT(id) DO NOTHING"#,
    "DELETE FROM projection_assertions WHERE ?1 IS NOT NULL",
];
