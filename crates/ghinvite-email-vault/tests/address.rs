//! Moved from gh-toolkit's `email_invitation_contract` tests (GM-07).
use ghinvite_email_vault::Address;

#[test]
fn transient_address_is_bounded_redacted_and_preserves_local_identity() {
    for (raw, expected) in [
        ("Private+tag@EXAMPLE.com", "Private+tag@example.com"),
        ("u@bücher.example", "u@xn--bcher-kva.example"),
    ] {
        let address = Address::parse(raw).unwrap();
        assert_eq!(address.as_str(), expected);
        assert_eq!(format!("{address:?}"), "Address([REDACTED])");
    }
    for raw in [
        "",
        "a",
        "a@@b.com",
        ".a@b.com",
        "a..b@b.com",
        "a@-b.com",
        "a@b_.com",
        "a@b.com\r\nBcc:x@y.com",
        "a@127.0.0.1",
        "a@b.com.",
        "a b@c.com",
        "ü@x.com",
    ] {
        let error = Address::parse(raw).unwrap_err();
        assert_eq!(error.to_string(), "invalid email address");
        assert!(!format!("{error:?}").contains('@'));
    }
    assert!(Address::parse(&format!("{}@x.com", "a".repeat(65))).is_err());
}
