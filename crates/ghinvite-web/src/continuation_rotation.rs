//! Operator-controlled, bounded continuation rewrap and independent verification.
//! A completion is a point-in-time result at one database mutation epoch, not
//! permission to retire a KEK while older writers or deployments remain active.
use anykms_core::{Ciphertext, ErrorKind};
use ghinvite_core::purpose;
use ghinvite_core::storage::{
    ContinuationStorage,
    continuation_rotation::{PAGE_SIZE, Phase, ScanRow},
};
use kekring::{Aad, Code, Keyring};
use serde::{Deserialize, Serialize};

const TOKEN_SCOPE: &str = "operator-continuation-rotation-v1";
const MAX_PASS_SECONDS: i64 = 3600;
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Counts {
    pub checked: u64,
    pub old: u64,
    pub rewrapped: u64,
    pub bad: u64,
}
#[derive(Serialize, Deserialize)]
struct Progress {
    version: u8,
    operator: u64,
    fingerprint: String,
    phase: Phase,
    started_at: i64,
    epoch: i64,
    after: Option<i64>,
    counts: Counts,
}
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RowFailure {
    InvalidSize,
    InvalidEnvelope,
    UnknownKeyVersion,
    RemovedKeyVersion,
    InvalidContext,
}
impl RowFailure {
    fn code(&self) -> &'static str {
        match self {
            Self::InvalidSize => "invalid_size",
            Self::InvalidEnvelope => "invalid_envelope",
            Self::UnknownKeyVersion => "unknown_key_version",
            Self::RemovedKeyVersion => "removed_key_version",
            Self::InvalidContext => "invalid_context",
        }
    }
}
#[derive(Serialize)]
pub struct Issue {
    pub row_id: i64,
    pub kind: RowFailure,
}
#[derive(Serialize)]
pub struct Report {
    pub phase: Phase,
    pub phase_finished: bool,
    pub complete: bool,
    pub observed_revision: i64,
    pub observed_at: i64,
    pub keyring_fingerprint: String,
    pub current_version: u32,
    pub counts: Counts,
    pub issues: Vec<Issue>,
    pub next: Option<String>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    InvalidProgress,
    RestartRequired,
    Unavailable,
}
fn context(operator: u64) -> Result<Aad, Failure> {
    Aad::try_from(&purpose::AttemptContinuation {
        scope: TOKEN_SCOPE,
        binding: &operator.to_string(),
    })
    .map_err(|_| Failure::Unavailable)
}
fn encode(ring: &Keyring, value: &Progress) -> Result<String, Failure> {
    let bytes = serde_json::to_vec(value).map_err(|_| Failure::Unavailable)?;
    let ctx = context(value.operator)?;
    ring.envelope()
        .encrypt_sync(&bytes, &ctx)
        .map(|c| c.to_base64())
        .map_err(|_| Failure::Unavailable)
}
fn decode(
    ring: &Keyring,
    operator: u64,
    phase: Phase,
    token: &str,
    now: i64,
) -> Result<Progress, Failure> {
    if token.len() > 16384 {
        return Err(Failure::InvalidProgress);
    }
    let ct = Ciphertext::from_base64(token).map_err(|_| Failure::InvalidProgress)?;
    let ctx = context(operator)?;
    let bytes = ring
        .envelope()
        .decrypt_sync(&ct, &ctx)
        .map_err(|_| Failure::InvalidProgress)?;
    let p: Progress = serde_json::from_slice(&bytes).map_err(|_| Failure::InvalidProgress)?;
    if p.version != 1
        || p.operator != operator
        || p.phase != phase
        || p.fingerprint != ring.fingerprint().to_string()
        || now < p.started_at
        || now
            .checked_sub(p.started_at)
            .is_none_or(|age| age > MAX_PASS_SECONDS)
        || p.epoch < 0
    {
        return Err(Failure::InvalidProgress);
    }
    Ok(p)
}
fn row_context(row: &ScanRow) -> Result<Aad, RowFailure> {
    Aad::try_from(&purpose::AttemptContinuation {
        scope: row.scope.as_deref().ok_or(RowFailure::InvalidContext)?,
        binding: row.binding.as_deref().ok_or(RowFailure::InvalidContext)?,
    })
    .map_err(|_| RowFailure::InvalidContext)
}
fn inspect(
    ring: &Keyring,
    row: &ScanRow,
    phase: Phase,
) -> Result<(bool, Option<String>), RowFailure> {
    if row.oversized != 0 {
        return Err(RowFailure::InvalidSize);
    }
    let ct = Ciphertext::from_base64(row.payload.as_deref().ok_or(RowFailure::InvalidEnvelope)?)
        .map_err(|_| RowFailure::InvalidEnvelope)?;
    let ctx = row_context(row)?;
    let envelope = ring.envelope();
    // Rollout skew is a code on a DecryptFailed; a removed generation is its own kind.
    let classify = |e: anykms_core::Error| match (Code::of(&e), e.kind()) {
        (Some(Code::VersionAboveHighWaterMark), _) => RowFailure::UnknownKeyVersion,
        (_, ErrorKind::KeyVersionUnusable) => RowFailure::RemovedKeyVersion,
        _ => RowFailure::InvalidEnvelope,
    };
    // Verify the complete payload and its stored context even when no rewrap is
    // needed. Plaintext is zeroizing and immediately dropped, never deserialized.
    drop(envelope.decrypt_sync(&ct, &ctx).map_err(classify)?);
    let old = envelope.needs_rewrap(&ct).map_err(classify)?;
    let replacement = if old && phase == Phase::Rewrap {
        // Verified above against these exact immutable bytes. This operation
        // changes only the committed wrapping slot, not the payload/DEK/context.
        Some(
            envelope
                .rewrap_current_sync(&ct)
                .map_err(classify)?
                .to_base64(),
        )
    } else {
        None
    };
    Ok((old, replacement))
}
/// One bounded page. An unknown CAS acknowledgement aborts this progress token;
/// a fresh pass safely observes any already committed rewrap.
pub async fn page(
    storage: &dyn ContinuationStorage,
    ring: &Keyring,
    operator: u64,
    phase: Phase,
    token: Option<&str>,
    now: i64,
) -> Result<Report, Failure> {
    let prior = token
        .map(|t| decode(ring, operator, phase, t, now))
        .transpose()?;
    let after = prior.as_ref().and_then(|p| p.after);
    let rows = storage
        .continuation_rotation_page(after)
        .await
        .map_err(|_| Failure::Unavailable)?;
    if rows.is_empty() || rows.len() > PAGE_SIZE + 1 {
        return Err(Failure::Unavailable);
    }
    let epoch = rows[0].revision;
    if rows
        .iter()
        .any(|r| r.revision != epoch || r.validate().is_err())
    {
        return Err(Failure::Unavailable);
    }
    let mut progress = prior.unwrap_or(Progress {
        version: 1,
        operator,
        fingerprint: ring.fingerprint().to_string(),
        phase,
        started_at: now,
        epoch,
        after: None,
        counts: Counts::default(),
    });
    if progress.epoch != epoch {
        return Err(Failure::RestartRequired);
    }
    let mut issues = Vec::new();
    let more = rows.len() > PAGE_SIZE;
    for row in rows.iter().take(PAGE_SIZE).filter(|r| r.row_id.is_some()) {
        let id = row.row_id.unwrap();
        if progress.after.is_some_and(|old| id <= old) {
            return Err(Failure::Unavailable);
        }
        progress.counts.checked = progress
            .counts
            .checked
            .checked_add(1)
            .ok_or(Failure::Unavailable)?;
        match inspect(ring, row, phase) {
            Ok((old, replacement)) => {
                progress.counts.old += u64::from(old);
                if let Some(replacement) = replacement {
                    if !storage
                        .rewrap_attempt_continuation(row, &replacement, progress.epoch)
                        .await
                        .map_err(|_| Failure::Unavailable)?
                    {
                        return Err(Failure::RestartRequired);
                    }
                    progress.epoch = progress.epoch.checked_add(1).ok_or(Failure::Unavailable)?;
                    progress.counts.rewrapped += 1;
                }
            }
            Err(kind) => {
                tracing::warn!(target: "p1_keyring", event = "continuation_rotation_bad_row",
                    row_id = id, failure = kind.code(),
                    keyring_fingerprint = %ring.fingerprint(),
                    "continuation rotation row requires triage");
                progress.counts.bad += 1;
                issues.push(Issue { row_id: id, kind });
            }
        }
        progress.after = Some(id);
    }
    // Includes mutations during local crypto, insertion behind the cursor, and
    // deletion/reuse of an already scanned row. D1 must read from the primary.
    if storage
        .continuation_rotation_epoch()
        .await
        .map_err(|_| Failure::Unavailable)?
        != progress.epoch
    {
        return Err(Failure::RestartRequired);
    }
    let complete =
        !more && phase == Phase::Verify && progress.counts.old == 0 && progress.counts.bad == 0;
    let next = more.then(|| encode(ring, &progress)).transpose()?;
    Ok(Report {
        phase,
        phase_finished: !more,
        complete,
        observed_revision: progress.epoch,
        observed_at: now,
        keyring_fingerprint: progress.fingerprint,
        current_version: ring.current_version(),
        counts: progress.counts,
        issues,
        next,
    })
}

/// Test-only cross-platform verification through the real supplied adapter.
/// Never registered in the production Worker. Assertions contain fixed labels,
/// not ciphertext, plaintext or database diagnostics.
#[cfg(feature = "runtime-tests")]
pub async fn verify_runtime(storage: &dyn ContinuationStorage) -> Result<(), Failure> {
    fn ring(v: u32, keys: &[(u32, u8)]) -> Keyring {
        let mut b = Keyring::builder("p1");
        for (n, k) in keys {
            b = b.generation(*n, [*k; 32]);
        }
        b.current(v).build().unwrap()
    }
    let old = ring(1, &[(1, 11)]);
    let new = ring(2, &[(1, 11), (2, 22)]);
    for i in 0..6 {
        let binding = format!("runtime-{i}");
        let ctx = Aad::try_from(&purpose::AttemptContinuation {
            scope: "rotation-runtime",
            binding: &binding,
        })
        .unwrap();
        let value = old
            .envelope()
            .encrypt_sync(b"ROTATION_RUNTIME_PRIVATE_MARKER", &ctx)
            .unwrap()
            .to_base64();
        storage
            .retain_attempt_continuation("rotation-runtime", &binding, &binding, &value, 100, 0)
            .await
            .map_err(|_| Failure::Unavailable)?;
    }
    let before = storage
        .continuation_rotation_page(None)
        .await
        .map_err(|_| Failure::Unavailable)?;
    let stale = page(storage, &new, 42, Phase::Verify, None, 200).await?;
    assert_eq!(stale.counts.old, 4);
    assert!(!stale.complete);
    let first = page(storage, &new, 42, Phase::Rewrap, None, 200).await?;
    assert_eq!(first.counts.rewrapped, 4);
    assert!(!first.complete);
    assert!(matches!(
        page(storage, &new, 42, Phase::Verify, stale.next.as_deref(), 200).await,
        Err(Failure::RestartRequired)
    ));
    let second = page(storage, &new, 42, Phase::Rewrap, first.next.as_deref(), 200).await?;
    assert!(second.phase_finished);
    assert_eq!(second.counts.rewrapped, 6);
    assert!(!second.complete);
    let verify = page(storage, &new, 42, Phase::Verify, None, 200).await?;
    let done = page(
        storage,
        &new,
        42,
        Phase::Verify,
        verify.next.as_deref(),
        200,
    )
    .await?;
    assert!(done.complete);
    assert_eq!(done.counts.checked, 6);
    assert_eq!(done.counts.old, 0);
    assert_eq!(done.counts.bad, 0);
    let removed = ring(2, &[(2, 22)]);
    for row in storage
        .continuation_rotation_page(None)
        .await
        .map_err(|_| Failure::Unavailable)?
    {
        let prior = before.iter().find(|old| old.id == row.id).unwrap();
        assert_eq!(
            (&prior.scope, &prior.id, &prior.binding, prior.expires_at),
            (&row.scope, &row.id, &row.binding, row.expires_at)
        );
        let bytes = removed
            .envelope()
            .decrypt_sync(
                &Ciphertext::from_base64(row.payload.as_ref().unwrap()).unwrap(),
                &row_context(&row).map_err(|_| Failure::Unavailable)?,
            )
            .map_err(|_| Failure::Unavailable)?;
        assert!(&*bytes == b"ROTATION_RUNTIME_PRIVATE_MARKER");
    }
    // Corrupt a current-generation retained row through a real competing CAS.
    let row = storage
        .continuation_rotation_page(None)
        .await
        .map_err(|_| Failure::Unavailable)?
        .remove(0);
    assert!(
        storage
            .rewrap_attempt_continuation(&row, "ROTATION_RUNTIME_BAD_CIPHERTEXT", row.revision)
            .await
            .map_err(|_| Failure::Unavailable)?
    );
    assert!(
        !storage
            .rewrap_attempt_continuation(&row, "stale", row.revision + 1)
            .await
            .map_err(|_| Failure::Unavailable)?
    );
    let verify = page(storage, &new, 42, Phase::Verify, None, 200).await?;
    let done = page(
        storage,
        &new,
        42,
        Phase::Verify,
        verify.next.as_deref(),
        200,
    )
    .await?;
    assert!(!done.complete);
    assert_eq!(done.counts.bad, 1);
    assert!(
        storage
            .get_attempt_continuation("rotation-runtime", "runtime-0", 200)
            .await
            .map_err(|_| Failure::Unavailable)?
            .is_none()
    );
    Ok(())
}
