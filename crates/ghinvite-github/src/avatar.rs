//! Public GitHub avatar bytes. The URL is derived exclusively from a numeric
//! identity and the deployment's avatar base (GitHub's CDN unless a
//! non-production deployment configures another); no credential,
//! caller-supplied host, or redirect is accepted.
use crate::{HttpTransportDyn, Method, Request, ids::UserId};
use std::time::Duration;

/// Maximum buffered image size; oversized bodies are refused before exposure.
pub const MAX_AVATAR_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
/// Fixed failures for numeric-identity-bound avatar retrieval; no URL or response text is retained.
pub enum AvatarError {
    #[error("avatar identity unavailable")]
    /// The requested avatar cannot be bound to the supplied numeric GitHub user.
    Identity,
    #[error("avatar unavailable")]
    /// The upstream image could not be retrieved with the required transport guarantees.
    Unavailable,
    #[error("avatar format unavailable")]
    /// The body is not a supported, bounded image representation.
    Format,
}

impl AvatarError {
    /// Stable error code suitable for metrics and journals; no diagnostic values.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Identity => "gh:avatar:identity",
            Self::Unavailable => "gh:avatar:unavailable",
            Self::Format => "gh:avatar:format",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
/// A verified image representation suitable for an application-controlled response.
pub struct Avatar {
    /// Validated image MIME type; never an upstream arbitrary content-type string.
    pub content_type: &'static str,
    /// Bounded image bytes; applications choose their own cache and response headers.
    pub bytes: Vec<u8>,
}

/// GitHub's avatar CDN.
pub const DEFAULT_AVATAR_BASE: &str = "https://avatars.githubusercontent.com";

/// Canonical public CDN address for a verified numeric GitHub identity.
/// Never preserves an upstream or caller-supplied URL.
pub fn canonical_url(user: UserId) -> Option<String> {
    canonical_url_at(DEFAULT_AVATAR_BASE, user)
}

/// [`canonical_url`] at another avatar base (e.g. an emulator's), with or
/// without a trailing slash: `{base}/u/{id}?s=80&v=4`.
pub fn canonical_url_at(base: &str, user: UserId) -> Option<String> {
    (user.get() != 0).then(|| format!("{}/u/{}?s=80&v=4", base.trim_end_matches('/'), user.get()))
}

/// One public, credential-free CDN GET, with a five-second end-to-end deadline.
/// Transport implementations enforce their streaming response cap and prohibit
/// redirects; this helper accepts at most 256 KiB of recognized raster content.
pub async fn get(transport: &dyn HttpTransportDyn, user: UserId) -> Result<Avatar, AvatarError> {
    get_at(transport, DEFAULT_AVATAR_BASE, user).await
}

/// [`get`] from another avatar base ([`canonical_url_at`]).
pub async fn get_at(
    transport: &dyn HttpTransportDyn,
    base: &str,
    user: UserId,
) -> Result<Avatar, AvatarError> {
    let request = Request::new(
        Method::Get,
        canonical_url_at(base, user).ok_or(AvatarError::Identity)?,
    )
    .header("accept", "image/png,image/jpeg,image/webp")
    .timeout(Duration::from_secs(5));
    let response = transport
        .send(request)
        .await
        .map_err(|_| AvatarError::Unavailable)?;
    if response.status != 200 {
        return Err(AvatarError::Unavailable);
    }
    if response.body.is_empty() || response.body.len() > MAX_AVATAR_BYTES {
        return Err(AvatarError::Format);
    }
    let media = response
        .headers
        .get("content-type")
        .and_then(|value| value.split(';').next())
        .map(str::trim);
    let bytes = response.body;
    let content_type = match media {
        Some("image/png") if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => "image/png",
        Some("image/jpeg") if bytes.starts_with(b"\xff\xd8\xff") => "image/jpeg",
        Some("image/webp")
            if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" =>
        {
            "image/webp"
        }
        _ => return Err(AvatarError::Format),
    };
    Ok(Avatar {
        content_type,
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HttpTransport, Response};
    use std::sync::Mutex;
    struct Recording {
        response: Response,
        requests: Mutex<Vec<Request>>,
    }
    impl HttpTransport for Recording {
        async fn send(&self, request: Request) -> Result<Response, crate::TransportError> {
            self.requests.lock().unwrap().push(request);
            Ok(self.response.clone())
        }
    }
    #[tokio::test]
    async fn avatar_is_numeric_public_single_dispatch_and_rejects_non_raster_or_redirects() {
        for (status, media, bytes, accepted) in [
            (200, "image/png", b"\x89PNG\r\n\x1a\n".to_vec(), true),
            (200, "image/jpeg", b"\xff\xd8\xff\xe0".to_vec(), true),
            (200, "image/webp", b"RIFF0000WEBP".to_vec(), true),
            (
                200,
                "image/svg+xml",
                b"<svg onload='unsafe'/>".to_vec(),
                false,
            ),
            (200, "image/png", b"<html>unsafe</html>".to_vec(), false),
            (200, "text/html", b"\x89PNG\r\n\x1a\n".to_vec(), false),
            (302, "image/png", b"\x89PNG\r\n\x1a\n".to_vec(), false),
            (404, "image/png", vec![], false),
            (200, "image/png", vec![0; MAX_AVATAR_BYTES + 1], false),
        ] {
            let transport = Recording {
                response: Response {
                    status,
                    headers: [
                        ("content-type".into(), media.into()),
                        ("location".into(), "https://untrusted.test".into()),
                    ]
                    .into(),
                    body: bytes,
                },
                requests: Mutex::new(vec![]),
            };
            assert_eq!(
                get(&transport, UserId::new(42)).await.is_ok(),
                accepted,
                "status {status}, type {media}"
            );
            let requests = transport.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            let request = &requests[0];
            assert_eq!(
                request.url,
                "https://avatars.githubusercontent.com/u/42?s=80&v=4"
            );
            assert_eq!(request.method, Method::Get);
            assert_eq!(request.timeout, Duration::from_secs(5));
            assert_eq!(
                request.headers.len(),
                1,
                "no credentials, cookies or referrer"
            );
            assert!(request.body_for_transport().is_none());
        }
        let transport = Recording {
            response: Response {
                status: 200,
                headers: Default::default(),
                body: vec![],
            },
            requests: Mutex::new(vec![]),
        };
        assert_eq!(
            get(&transport, UserId::new(0)).await,
            Err(AvatarError::Identity)
        );
        assert!(transport.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn another_base_keeps_the_path_with_or_without_a_trailing_slash() {
        for base in ["http://127.0.0.1:9/avatars", "http://127.0.0.1:9/avatars/"] {
            assert_eq!(
                canonical_url_at(base, UserId::new(42)).as_deref(),
                Some("http://127.0.0.1:9/avatars/u/42?s=80&v=4")
            );
        }
        assert_eq!(canonical_url_at("http://127.0.0.1:9", UserId::new(0)), None);
    }
}
