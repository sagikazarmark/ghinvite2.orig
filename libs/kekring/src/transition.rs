//! Whether one descriptor may replace another in a deployment.
use crate::Keyring;
use std::fmt;
use subtle::ConstantTimeEq;

/// Why a descriptor may not replace another. Names versions, never material.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransitionError {
    /// The keyring was renamed: a rename is a new keyring (migrate with a re-encryption).
    Renamed,
    /// A generation present in both has different root material.
    RootChanged { version: u32 },
    /// The high-water mark went down.
    HighWaterMarkLowered { old: u32, new: u32 },
    /// A new generation reuses a version at or below the old high-water mark.
    VersionReused { version: u32, high_water_mark: u32 },
    /// The old current generation is gone: data it wrapped would become unreadable mid-rollout.
    CurrentRemoved { version: u32 },
    /// Promotion to a generation the old descriptor did not hold: readers must have it first.
    PromotedUnreleased { version: u32 },
    /// The current generation moved to a lower version.
    PromotedBackwards { old: u32, new: u32 },
}

impl fmt::Display for TransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Renamed => f.write_str("the keyring name changed"),
            Self::RootChanged { version } => {
                write!(f, "generation {version} has different root material")
            }
            Self::HighWaterMarkLowered { old, new } => {
                write!(f, "the high-water mark went down from {old} to {new}")
            }
            Self::VersionReused {
                version,
                high_water_mark,
            } => write!(
                f,
                "new generation {version} is not above the old high-water mark {high_water_mark}"
            ),
            Self::CurrentRemoved { version } => {
                write!(f, "the old current generation {version} was removed")
            }
            Self::PromotedUnreleased { version } => write!(
                f,
                "generation {version} is promoted in the same step that adds it"
            ),
            Self::PromotedBackwards { old, new } => {
                write!(f, "the current generation moved back from {old} to {new}")
            }
        }
    }
}

impl std::error::Error for TransitionError {}

/// Checks that `new` may replace `old` in one deployment step: same name, unchanged roots for
/// every retained version, a high-water mark that never goes down, new versions only above the
/// old high-water mark, the old current retained, and a promotion only forwards and only to a
/// generation `old` already held (add, deploy everywhere, then promote). Removing any other
/// generation is allowed. Identical descriptors pass.
///
/// # Errors
/// The first rule `new` breaks.
pub fn validate_transition(old: &Keyring, new: &Keyring) -> Result<(), TransitionError> {
    if old.name() != new.name() {
        return Err(TransitionError::Renamed);
    }
    for v in new.versions() {
        match old.root(v) {
            Some(root) => {
                if !bool::from(root.ct_eq(new.root(v).expect("listed"))) {
                    return Err(TransitionError::RootChanged { version: v });
                }
            }
            None if v <= old.high_water_mark() => {
                return Err(TransitionError::VersionReused {
                    version: v,
                    high_water_mark: old.high_water_mark(),
                });
            }
            None => {}
        }
    }
    if new.high_water_mark() < old.high_water_mark() {
        return Err(TransitionError::HighWaterMarkLowered {
            old: old.high_water_mark(),
            new: new.high_water_mark(),
        });
    }
    let (was, is) = (old.current_version(), new.current_version());
    if new.root(was).is_none() {
        return Err(TransitionError::CurrentRemoved { version: was });
    }
    if is < was {
        return Err(TransitionError::PromotedBackwards { old: was, new: is });
    }
    if is != was && old.root(is).is_none() {
        return Err(TransitionError::PromotedUnreleased { version: is });
    }
    Ok(())
}
