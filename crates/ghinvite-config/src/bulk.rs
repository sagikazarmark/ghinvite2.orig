//! Explicit M2 composition and a vault-only deployment keyring. Session keys
//! remain web-owned; workflow roots never need the web session keyring.
use crate::apps::AppDeploymentError;
use std::sync::Arc;
use zeroize::Zeroizing;
pub fn enabled(value: Option<&str>) -> Result<bool, AppDeploymentError> {
    match value {
        None | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(AppDeploymentError {
            field: "GH_BULK_ENABLED",
        }),
    }
}
pub fn email_keyring(
    required: bool,
    value: Option<String>,
) -> Result<Option<Arc<anykms_keyring::Keyring>>, AppDeploymentError> {
    let error = || AppDeploymentError {
        field: "P1_EMAIL_KEYRING",
    };
    let Some(value) = value else {
        return if required { Err(error()) } else { Ok(None) };
    };
    let value = Zeroizing::new(value);
    let ring = anykms_keyring::Keyring::parse(&value).map_err(|_| error())?;
    if ring.name() != "p1-email" {
        return Err(error());
    }
    Ok(Some(Arc::new(ring)))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dedicated_ring_validates_name_and_key_check_value() {
        use base64::Engine as _;
        for name in ["p1-email", "p1"] {
            let ring = anykms_keyring::Keyring::builder(name)
                .generation(1, [7; 32])
                .unwrap()
                .current(1)
                .unwrap()
                .build()
                .unwrap();
            let kcv = ring
                .key_check_value(1)
                .unwrap()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            let descriptor = format!(
                "akr1;name={name};current=1;k1={}.{kcv}",
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7; 32])
            );
            let parsed = email_keyring(true, Some(descriptor));
            assert_eq!(parsed.is_ok(), name == "p1-email");
        }
        let wrong = "akr1;name=p1-email;current=1;k1=BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc.0000000000000000";
        assert_eq!(
            email_keyring(true, Some(wrong.into())).unwrap_err().field,
            "P1_EMAIL_KEYRING"
        );
    }
    #[test]
    fn explicit_flag_and_secret_fail_with_field_names_only() {
        assert_eq!(enabled(None), Ok(false));
        assert_eq!(enabled(Some("false")), Ok(false));
        assert_eq!(enabled(Some("true")), Ok(true));
        for value in ["", "1", "True", " true", "secret-value"] {
            assert_eq!(enabled(Some(value)).unwrap_err().field, "GH_BULK_ENABLED");
        }
        assert!(email_keyring(false, None).unwrap().is_none());
        assert_eq!(
            email_keyring(true, None).unwrap_err().field,
            "P1_EMAIL_KEYRING"
        );
        for required in [false, true] {
            let error =
                email_keyring(required, Some("PRIVATE_EMAIL_KEY_MARKER".into())).unwrap_err();
            assert_eq!(
                error.to_string(),
                "invalid or missing deployment configuration: P1_EMAIL_KEYRING"
            );
        }
    }
}
