//! Input-bound HTTP attempt fence, shared by SQLite and D1.
use crate::delivery::CreateCommand;
use serde::Deserialize;

/// Bind invitation ID and [`encode`]d command. Returns a row only when a new
/// generation is claimed.
pub const CLAIM: &str = "INSERT INTO delivery_attempts(invitation_id, command) VALUES (?1, ?2) ON CONFLICT(invitation_id) DO UPDATE SET generation = generation + 1, retryable = 0 WHERE retryable = 1 AND command = excluded.command RETURNING generation";
pub const COMMAND: &str = "SELECT command FROM delivery_attempts WHERE invitation_id = ?1";
/// Bind invitation ID and generation.
pub const REJECT: &str =
    "UPDATE delivery_attempts SET retryable = 1 WHERE invitation_id = ?1 AND generation = ?2";
/// Returns a row only for a fenced (non-retryable) attempt.
pub const FENCED: &str =
    "SELECT 1 AS present FROM delivery_attempts WHERE invitation_id = ?1 AND retryable = 0";

#[derive(Debug, Deserialize)]
pub struct Generation {
    pub generation: u64,
}

#[derive(Debug, Deserialize)]
pub struct Command {
    pub command: String,
}

pub fn encode(command: &CreateCommand) -> super::Result<String> {
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
