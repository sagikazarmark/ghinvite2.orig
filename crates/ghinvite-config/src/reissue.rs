//! Explicit matched web/workflow administrator-reissue activation.
use crate::apps::{AppDeploymentError, bad};
pub fn enabled(value: Option<&str>) -> Result<bool, AppDeploymentError> {
    match value {
        None | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(bad("GH_REISSUE_ENABLED")),
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn explicit_boolean_and_fail_closed() {
        assert!(!super::enabled(None).unwrap());
        assert!(!super::enabled(Some("false")).unwrap());
        assert!(super::enabled(Some("true")).unwrap());
        for value in ["", "TRUE", "1", " true", "false "] {
            assert_eq!(
                super::enabled(Some(value)).unwrap_err().field,
                "GH_REISSUE_ENABLED"
            );
        }
    }
}
