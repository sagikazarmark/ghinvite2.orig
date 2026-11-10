//! Observation clocks are retained evidence, independent of projection time.
use super::*;
use ghinvite_core::storage::organization_telemetry::{
    OrganizationStepTelemetry, OrganizationTelemetryStep,
};

fn observed(value: &mut Retained, step: &str, at: DateTime<Utc>) {
    if at < value.snapshot.create.command.approved_at {
        return;
    }
    let Some(record) = value.steps.get_mut(step) else {
        return;
    };
    record.last_observed_at = Some(record.last_observed_at.map_or(at, |old| old.max(at)));
}

/// A failed team detail/membership read is not a successful observation of that
/// team's access. Other successfully verified steps remain independently fresh.
pub(super) fn memberships(value: &mut Retained, read: &protocol::Ready, at: DateTime<Utc>) {
    observed(value, "org", at);
    for (id, team) in &read.teams {
        if matches!(
            team,
            protocol::TeamRead::Missing | protocol::TeamRead::Present { .. }
        ) {
            observed(value, &format!("team:{id}"), at);
        }
    }
    if let protocol::OrganizationRead::Pending { upstream_id, .. } = read.organization {
        invitation(value, upstream_id, at);
    }
}

/// The organization invitation is shared evidence only for attached teams with
/// that exact retained upstream identity. Independent pending team memberships
/// never inherit the organization's observation clock.
pub(super) fn invitation(value: &mut Retained, upstream_id: u64, at: DateTime<Utc>) {
    observed(value, "org", at);
    let ids = value
        .snapshot
        .teams
        .iter()
        .filter_map(|(id, team)| {
            matches!(team.create.outcome, TeamCreateOutcome::Created {
            via: TeamCreation::OrganizationInvitation { upstream_id: owned }
        } if owned == upstream_id)
            .then_some(*id)
        })
        .collect::<Vec<_>>();
    for id in ids {
        observed(value, &format!("team:{id}"), at);
    }
}

pub(super) async fn send(ctx: &ObjectContext<'_>, value: &Retained) -> Result<(), TerminalError> {
    let command = &value.snapshot.create.command;
    for (name, record) in &value.steps {
        if record.unknown_since.is_none() && record.last_observed_at.is_none() {
            continue;
        }
        let step = if name == "org" {
            OrganizationTelemetryStep::Organization
        } else {
            OrganizationTelemetryStep::Team {
                team_id: name
                    .strip_prefix("team:")
                    .and_then(|id| id.parse().ok())
                    .ok_or_else(conflict)?,
            }
        };
        ctx.object_client::<OrganizationProjectionClient>(ctx.key())
            .telemetry(Json(OrganizationStepTelemetry {
                request_id: command.request_id,
                org_id: command.organization.org_id,
                delivery_generation: command.delivery_generation,
                account_id: command.account_id,
                step,
                unknown_since: record.unknown_since,
                last_observed_at: record.last_observed_at,
            }))
            .send()
            .await?;
    }
    Ok(())
}
