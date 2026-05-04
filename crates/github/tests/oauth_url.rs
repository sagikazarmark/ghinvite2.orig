//! Integration smoke for the public OAuth URL builder. Runs against
//! `crates/github`'s public re-exports — i.e. exercises the same import shape
//! the web binary will use in Plan 4.

use github::oauth::{AuthorizeUrl, OAuthConfig};

#[test]
fn authorize_url_is_crate_public() {
    let cfg = OAuthConfig {
        client_id: "Iv1.abc".into(),
        client_secret: "secret".into(),
        redirect_uri: "https://example.test/oauth/callback".into(),
    };
    let a = AuthorizeUrl::build(&cfg, "csrf", &[]).unwrap();
    assert!(a.url.starts_with("https://github.com/login/oauth/authorize?"));
    assert_eq!(a.state, "csrf");
}
