//! Per-step monotonic observations, separate from immutable delivery receipts.
use super::{Error, Result};
use crate::RequestId;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OrganizationTelemetryStep {
    Organization,
    Team { team_id: u64 },
}
impl OrganizationTelemetryStep {
    pub fn key(self) -> String {
        match self {
            Self::Organization => "org".into(),
            Self::Team { team_id } => format!("team:{team_id}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OrganizationStepTelemetry {
    pub request_id: RequestId,
    pub org_id: u64,
    pub delivery_generation: u64,
    pub account_id: u64,
    pub step: OrganizationTelemetryStep,
    pub unknown_since: Option<DateTime<Utc>>,
    pub last_observed_at: Option<DateTime<Utc>>,
}
impl OrganizationStepTelemetry {
    pub fn key(&self) -> String {
        format!(
            "org:{}",
            crate::delivery::delivery_key(self.request_id, self.org_id, self.delivery_generation)
        )
    }
}

pub fn encode(value: &OrganizationStepTelemetry) -> Result<String> {
    let mut ids = vec![value.org_id, value.delivery_generation, value.account_id];
    let team_id = match value.step {
        OrganizationTelemetryStep::Organization => None,
        OrganizationTelemetryStep::Team { team_id } => {
            ids.push(team_id);
            Some(team_id)
        }
    };
    if ids
        .iter()
        .any(|id| *id == 0 || *id > super::integer::MAX_EXACT)
        || value.org_id != value.account_id
    {
        return Err(Error::ProjectionInvariant(
            "invalid organization telemetry identity".into(),
        ));
    }
    let time = |date: DateTime<Utc>| date.to_rfc3339_opts(SecondsFormat::Nanos, true);
    Ok(serde_json::json!({
        "key":value.key(),"step":value.step.key(),"team_id":team_id,
        "request_id":value.request_id,"org_id":value.org_id,
        "delivery_generation":value.delivery_generation,"account_id":value.account_id,
        "unknown_since":value.unknown_since.map(time),"last_observed_at":value.last_observed_at.map(time)
    }).to_string())
}

pub fn statements() -> Vec<String> {
    // Normalize legacy UTC timestamp precision before lexicographic comparison.
    let created = "(substr(d.created_at,1,19) || '.' || substr(CASE WHEN substr(d.created_at,20,1)='.' THEN replace(replace(substr(d.created_at,21),'+00:00',''),'Z','') ELSE '' END || '000000000',1,9) || 'Z')";
    vec![
        "INSERT INTO projection_assertions(dependency) SELECT EXISTS(SELECT 1 FROM deliveries WHERE id=json_extract(?1,'$.key'))".into(),
        format!(r#"INSERT INTO projection_assertions(invariant) SELECT EXISTS(
            SELECT 1 FROM deliveries d JOIN invitation_requests r ON r.id=d.request_id
            LEFT JOIN invitation_links l ON l.id=r.invitation_link_id
            LEFT JOIN bulk_batches b ON b.id=r.batch_id
            JOIN delivery_outcomes o ON o.delivery_id=d.id
            WHERE d.id=json_extract(?1,'$.key') AND d.kind='org'
            AND d.request_id=json_extract(?1,'$.request_id') AND d.target_id=json_extract(?1,'$.org_id')
            AND d.delivery_generation=json_extract(?1,'$.delivery_generation') AND COALESCE(l.account_id,b.account_id)=json_extract(?1,'$.account_id')
            AND (json_extract(?1,'$.team_id') IS NULL OR EXISTS(
                SELECT 1 FROM json_each(o.receipt,'$.create.command.teams') t
                WHERE json_extract(t.value,'$.team_id')=json_extract(?1,'$.team_id')))
            AND (json_extract(?1,'$.unknown_since') IS NULL OR json_extract(?1,'$.unknown_since')>={created})
            AND (json_extract(?1,'$.last_observed_at') IS NULL OR json_extract(?1,'$.last_observed_at')>={created})
            AND NOT EXISTS(SELECT 1 FROM organization_step_telemetry t WHERE t.delivery_id=d.id
                AND t.step=json_extract(?1,'$.step') AND t.unknown_since IS NOT NULL
                AND json_extract(?1,'$.unknown_since') IS NOT NULL AND t.unknown_since!=json_extract(?1,'$.unknown_since'))
        )"#),
        r#"INSERT INTO organization_step_telemetry(delivery_id,step,unknown_since,last_observed_at)
        VALUES(json_extract(?1,'$.key'),json_extract(?1,'$.step'),json_extract(?1,'$.unknown_since'),json_extract(?1,'$.last_observed_at'))
        ON CONFLICT(delivery_id,step) DO UPDATE SET
        unknown_since=COALESCE(organization_step_telemetry.unknown_since,excluded.unknown_since),
        last_observed_at=CASE WHEN excluded.last_observed_at IS NOT NULL AND (organization_step_telemetry.last_observed_at IS NULL OR excluded.last_observed_at>organization_step_telemetry.last_observed_at)
        THEN excluded.last_observed_at ELSE organization_step_telemetry.last_observed_at END"#.into(),
        "DELETE FROM projection_assertions WHERE ?1 IS NOT NULL".into(),
    ]
}
