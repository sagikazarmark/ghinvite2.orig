//! Input-bound HTTP attempt fence, shared by SQLite and D1.
use crate::delivery::CreateCommand;
use serde::Deserialize;

/// Bind the canonical delivery key and the exact encoded command.
pub const CLAIM: &str = "INSERT INTO delivery_attempts(delivery_key,step,command) SELECT ?1,'repo',?2 WHERE NOT EXISTS(SELECT 1 FROM delivery_attempts WHERE step='repo' AND json_extract(command,'$.invitation_id')=json_extract(?2,'$.invitation_id') AND command!=?2) ON CONFLICT(delivery_key,step) DO UPDATE SET attempt_generation=attempt_generation+1,retryable=0 WHERE retryable=1 AND command=excluded.command RETURNING attempt_generation AS generation";
pub const COMMAND: &str = "SELECT command FROM delivery_attempts WHERE step='repo' AND (delivery_key=?1 OR json_extract(command,'$.invitation_id')=json_extract(?2,'$.invitation_id'))";
pub const REJECT: &str = "UPDATE delivery_attempts SET retryable=1 WHERE delivery_key=?1 AND step='repo' AND command=?2 AND attempt_generation=?3";
pub const FENCED: &str =
    "SELECT command,retryable FROM delivery_attempts WHERE delivery_key=?1 AND step='repo'";

#[derive(Debug, Deserialize)]
pub struct Fence {
    pub command: String,
    pub retryable: i64,
}
pub fn fenced(row: Option<Fence>, encoded: &str) -> super::Result<bool> {
    match row {
        None => Ok(false),
        Some(row) if row.command == encoded => Ok(row.retryable == 0),
        Some(_) => Err(super::Error::ProjectionInvariant(
            "delivery attempt conflict".into(),
        )),
    }
}

#[derive(Debug, Deserialize)]
pub struct Generation {
    pub generation: u64,
}

#[derive(Debug, Deserialize)]
pub struct Command {
    pub command: String,
}

pub fn encode(command: &CreateCommand) -> super::Result<String> {
    super::integer::validate(command)?;
    if command.delivery_generation == 0 || !(1..=100).contains(&command.budget_share_pct) {
        return Err(super::Error::ProjectionInvariant(
            "zero delivery generation".into(),
        ));
    }
    serde_json::to_string(command).map_err(|e| super::Error::Corrupt(e.to_string()))
}

/// The claim's outcome once the retained command is read back: a different
/// command under the same invitation ID is an invariant violation.
pub fn claimed(
    generation: Option<Generation>,
    retained: Command,
    encoded: &str,
) -> super::Result<Option<u64>> {
    if retained.command != encoded {
        return Err(super::Error::ProjectionInvariant(
            "delivery attempt conflict".into(),
        ));
    }
    Ok(generation.map(|g| g.generation))
}

/// A separately fenced cancellation. The complete original create identity and
/// first cancellation reason are immutable, independently of the create fence.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, Deserialize, schemars::JsonSchema)]
pub struct CancelCommand {
    #[serde(flatten)]
    pub create: CreateCommand,
    pub upstream_id: u64,
    pub reason: crate::delivery::SettlementReason,
}

pub const CANCEL_CLAIM: &str = "INSERT INTO delivery_attempts(delivery_key,step,command) SELECT ?1,'cancel',?2 WHERE NOT EXISTS(SELECT 1 FROM delivery_attempts WHERE step='cancel' AND json_extract(command,'$.invitation_id')=json_extract(?2,'$.invitation_id') AND command!=?2) ON CONFLICT(delivery_key,step) DO UPDATE SET attempt_generation=attempt_generation+1,retryable=0 WHERE retryable=1 AND command=excluded.command RETURNING attempt_generation AS generation";
pub const CANCEL_COMMAND: &str = "SELECT command FROM delivery_attempts WHERE step='cancel' AND (delivery_key=?1 OR json_extract(command,'$.invitation_id')=json_extract(?2,'$.invitation_id'))";
/// Exact command and generation binding prevents stale rejection acknowledgements
/// or another invitation's input from releasing a newer uncertain DELETE.
pub const CANCEL_REJECT: &str = "UPDATE delivery_attempts SET retryable=1 WHERE delivery_key=?1 AND step='cancel' AND command=?2 AND attempt_generation=?3";

pub fn encode_cancel(command: &CancelCommand) -> super::Result<String> {
    encode(&command.create)?;
    if command.upstream_id == 0 || command.upstream_id > super::integer::MAX_EXACT {
        return Err(super::Error::ProjectionInvariant(
            "invalid upstream invitation".into(),
        ));
    }
    serde_json::to_string(command).map_err(|e| super::Error::Corrupt(e.to_string()))
}

pub const CANCEL_FENCED: &str =
    "SELECT command,retryable FROM delivery_attempts WHERE delivery_key=?1 AND step='cancel'";
