//! Fixture-only replacement at the invitation capability's entropy boundary.
//! No request input controls this seam. Ordinary builds reject the fixture
//! binding; dedicated fixture builds require it and never silently fall back.

const CONFIGURATION_ERROR: &str = "invitation entropy configuration unavailable";

/// Called by the Worker composition root before serving application routes.
pub fn configure(binding: Option<&str>) -> Result<(), &'static str> {
    #[cfg(not(feature = "link-secret-fixture"))]
    return if binding.is_none() {
        Ok(())
    } else {
        Err(CONFIGURATION_ERROR)
    };

    #[cfg(feature = "link-secret-fixture")]
    {
        if binding != Some("known-marker-v1") {
            return Err(CONFIGURATION_ERROR);
        }
        let mut next = NEXT.lock().map_err(|_| CONFIGURATION_ERROR)?;
        // Configuration runs for every request. Never reset the sequence.
        next.get_or_insert(1);
        Ok(())
    }
}

#[cfg(feature = "link-secret-fixture")]
static NEXT: std::sync::Mutex<Option<u64>> = std::sync::Mutex::new(None);

pub(crate) fn fill(bytes: &mut [u8; 32]) -> Result<(), ()> {
    #[cfg(not(feature = "link-secret-fixture"))]
    return getrandom::fill(bytes).map_err(|_| ());

    #[cfg(feature = "link-secret-fixture")]
    {
        let mut next = NEXT.lock().map_err(|_| ())?;
        let sequence = next.as_mut().ok_or(())?;
        if *sequence > 99_999_999_999_999 {
            return Err(());
        }
        bytes[..18].copy_from_slice(b"LINKSECRET_MARKER_");
        let mut digits = *sequence;
        for byte in bytes[18..].iter_mut().rev() {
            *byte = b'0' + (digits % 10) as u8;
            digits /= 10;
        }
        *sequence += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(feature = "link-secret-fixture"))]
    fn ordinary_build_rejects_every_fixture_binding() {
        assert!(configure(None).is_ok());
        for value in ["", "known-marker-v1", "arbitrary"] {
            assert_eq!(configure(Some(value)), Err(CONFIGURATION_ERROR));
        }
    }

    #[test]
    #[cfg(feature = "link-secret-fixture")]
    fn fixture_requires_startup_and_exercises_production_secret_generation() {
        use crate::link_secret::LinkSecret;
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        assert!(LinkSecret::generate().is_err());
        assert!(configure(None).is_err());
        assert!(configure(Some("arbitrary")).is_err());
        for n in [1, 2] {
            configure(Some("known-marker-v1")).unwrap();
            let secret = LinkSecret::generate().unwrap();
            let expected = format!("LINKSECRET_MARKER_{n:014}");
            assert_eq!(
                secret.show_once().as_str(),
                URL_SAFE_NO_PAD.encode(expected)
            );
        }
    }
}
