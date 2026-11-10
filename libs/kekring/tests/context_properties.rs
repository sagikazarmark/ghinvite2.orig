use anykms_keyring::EncryptionContext;
use proptest::prelude::*;
anykms_keyring::purpose! { pub struct Fields<'a> = "properties" { left:&'a str, right:&'a str, number:u64, optional:Option<&'a str> } }
fn make(left: &str, right: &str, number: u64, optional: Option<&str>) -> EncryptionContext {
    EncryptionContext::try_from(&Fields {
        left,
        right,
        number,
        optional,
    })
    .unwrap()
}
proptest! {
 #![proptest_config(ProptestConfig { cases: 1_000_000, .. ProptestConfig::default() })]
 #[test] fn canonical_order_and_injectivity(
 a in any::<u64>(), b in any::<u64>(),
 left in prop::collection::vec(any::<char>(),1..6), right in prop::collection::vec(any::<char>(),1..6), optional in any::<bool>()
 ) {
 let left:String=left.into_iter().collect();let right:String=right.into_iter().collect();let opt=optional.then_some("optional\0é");
 let first=make(&left,&right,a,opt);
 let mut builder=EncryptionContext::builder::<Fields>().u64("number",a).str("right",&right).str("left",&left);
 if let Some(v)=opt {builder=builder.str("optional",v);}
 let reordered=builder.build().unwrap();
 let other=make(&right,&left,b,opt);
 prop_assert_eq!(first.as_aad(),reordered.as_aad());
 prop_assert_eq!(first.as_aad()==other.as_aad(),a==b && left==right);
 let toggled=make(&left,&right,a,(!optional).then_some("optional\0é"));
 prop_assert_ne!(first.as_aad(),toggled.as_aad());
 }
}
#[test]
fn ambiguity_and_utf8_boundaries() {
    for (a, b, c, d) in [
        ("ab", "c", "a", "bc"),
        ("a\0", "b", "a", "\0b"),
        ("é", "e\u{301}", "e\u{301}", "é"),
        ("a=b;c", "d", "a", "b;c=d"),
    ] {
        assert_ne!(make(a, b, 0, None).as_aad(), make(c, d, 0, None).as_aad());
    }
    let valid = "é".repeat(127) + "a";
    assert_eq!(valid.len(), 255);
    assert!(
        EncryptionContext::try_from(&Fields {
            left: &valid,
            right: "b",
            number: 0,
            optional: None
        })
        .is_ok()
    );
    let invalid = "é".repeat(128);
    assert!(
        EncryptionContext::try_from(&Fields {
            left: &invalid,
            right: "b",
            number: 0,
            optional: None
        })
        .is_err()
    );
}
