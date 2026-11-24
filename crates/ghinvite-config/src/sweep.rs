//! The web's hourly sweep watchdog (#86). It is on for development roots and
//! test harnesses; a production root keeps it off until its operator sets it
//! (production enablement is #61's decision).
use crate::apps::{AppDeploymentError, Deployment, bad};

pub const WATCHDOG_FIELD: &str = "GH_SWEEP_WATCHDOG_ENABLED";

pub fn watchdog_enabled(
    deployment: Deployment,
    value: Option<&str>,
) -> Result<bool, AppDeploymentError> {
    match value {
        None => Ok(deployment == Deployment::Development),
        Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(bad(WATCHDOG_FIELD)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn on_for_development_off_for_production_and_fail_closed() {
        assert!(watchdog_enabled(Deployment::Development, None).unwrap());
        assert!(!watchdog_enabled(Deployment::Production, None).unwrap());
        for deployment in [Deployment::Development, Deployment::Production] {
            assert!(watchdog_enabled(deployment, Some("true")).unwrap());
            assert!(!watchdog_enabled(deployment, Some("false")).unwrap());
            for value in ["", "TRUE", "1", " true", "false "] {
                assert_eq!(
                    watchdog_enabled(deployment, Some(value)).unwrap_err().field,
                    WATCHDOG_FIELD
                );
            }
        }
    }
}
