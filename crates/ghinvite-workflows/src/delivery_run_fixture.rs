//! Explicit deployment-only fixture for losing three guarded-run results.
//! Ordinary builds reject its binding; request input cannot configure it.

pub fn configure(binding: Option<&str>) -> Result<(), &'static str> {
    let Some(binding) = binding else {
        return Ok(());
    };
    #[cfg(not(feature = "runtime-tests"))]
    {
        let _ = binding;
        Err("delivery run fixture is unavailable")
    }
    #[cfg(feature = "runtime-tests")]
    {
        if binding != "three-losses-v1" {
            return Err("delivery run fixture is unavailable");
        }
        STATE.get_or_init(|| State {
            remaining: std::sync::atomic::AtomicUsize::new(3),
            losses: std::sync::atomic::AtomicUsize::new(0),
        });
        Ok(())
    }
}

#[cfg(feature = "runtime-tests")]
struct State {
    remaining: std::sync::atomic::AtomicUsize,
    losses: std::sync::atomic::AtomicUsize,
}
#[cfg(feature = "runtime-tests")]
static STATE: std::sync::OnceLock<State> = std::sync::OnceLock::new();

#[cfg(feature = "runtime-tests")]
pub(crate) fn lose_confirmed_result() -> bool {
    use std::sync::atomic::Ordering;
    let Some(state) = STATE.get() else {
        return false;
    };
    if state
        .remaining
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_ok()
    {
        state.losses.fetch_add(1, Ordering::SeqCst);
        true
    } else {
        false
    }
}

#[cfg(feature = "runtime-tests")]
pub fn losses() -> usize {
    STATE
        .get()
        .map_or(0, |s| s.losses.load(std::sync::atomic::Ordering::SeqCst))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(not(feature = "runtime-tests"))]
    fn production_rejects_fixture_binding() {
        assert!(configure(None).is_ok());
        for value in ["", "wrong", "three-losses-v1"] {
            assert!(configure(Some(value)).is_err());
        }
    }
    #[test]
    #[cfg(feature = "runtime-tests")]
    fn explicit_fixture_is_bounded_and_never_rearmed_by_request_bootstrap() {
        assert!(configure(None).is_ok());
        assert!(!lose_confirmed_result());
        assert!(configure(Some("wrong")).is_err());
        for expected in 1..=3 {
            configure(Some("three-losses-v1")).unwrap();
            assert!(lose_confirmed_result());
            assert_eq!(losses(), expected);
        }
        configure(Some("three-losses-v1")).unwrap();
        assert!(!lose_confirmed_result());
        assert_eq!(losses(), 3);
    }
}
