//! The canonical AAD is order-independent and injective over field values.
use kekring::{Aad, Purpose};
use proptest::prelude::*;

struct Fields;
impl Purpose for Fields {
    const NAME: &'static str = "properties";
    const FIELDS: &'static [&'static str] = &["left", "right", "number"];
}

fn make(left: &str, right: &str, number: u64) -> Option<Aad> {
    Aad::builder::<Fields>()
        .str("left", left)
        .str("right", right)
        .u64("number", number)
        .build()
        .ok()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 20_000, .. ProptestConfig::default() })]
    #[test]
    fn canonical_order_and_injectivity(
        a in any::<u64>(), b in any::<u64>(),
        left in "[^\\p{Cc}]{1,6}", right in "[^\\p{Cc}]{1,6}",
    ) {
        let first = make(&left, &right, a).unwrap();
        let reordered = Aad::builder::<Fields>()
            .u64("number", a)
            .str("right", &right)
            .str("left", &left)
            .build()
            .unwrap();
        prop_assert_eq!(first.as_bytes(), reordered.as_bytes());
        let other = make(&right, &left, b).unwrap();
        prop_assert_eq!(first.as_bytes() == other.as_bytes(), a == b && left == right);
    }
}

#[test]
fn ambiguity_and_utf8_boundaries() {
    for (a, b, c, d) in [
        ("ab", "c", "a", "bc"),
        ("é", "e\u{301}", "e\u{301}", "é"),
        ("a=b;c", "d", "a", "b;c=d"),
    ] {
        assert_ne!(
            make(a, b, 0).unwrap().as_bytes(),
            make(c, d, 0).unwrap().as_bytes()
        );
    }
    let valid = "é".repeat(127) + "a";
    assert_eq!(valid.len(), 255);
    assert!(make(&valid, "b", 0).is_some());
    assert!(make(&"é".repeat(128), "b", 0).is_none());
    assert!(make("a\0", "b", 0).is_none());
}
