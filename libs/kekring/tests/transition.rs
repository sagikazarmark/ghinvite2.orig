use kekring::{Keyring, TransitionError, validate_transition};

/// `generations` as `(version, root byte)`.
fn ring(name: &str, current: u32, hwm: u32, generations: &[(u32, u8)]) -> Keyring {
    let mut b = Keyring::builder(name).current(current).high_water_mark(hwm);
    for (v, byte) in generations {
        b = b.generation(*v, [*byte; 32]);
    }
    b.build().unwrap()
}

#[test]
fn the_rotation_sequence_passes() {
    let steps = [
        ring("t", 1, 1, &[(1, 1)]),
        ring("t", 1, 2, &[(1, 1), (2, 2)]), // add
        ring("t", 2, 2, &[(1, 1), (2, 2)]), // promote
        ring("t", 2, 2, &[(2, 2)]),         // remove
        ring("t", 2, 3, &[(2, 2), (3, 3)]), // add the next
    ];
    for pair in steps.windows(2) {
        assert_eq!(validate_transition(&pair[0], &pair[1]), Ok(()));
    }
    assert_eq!(validate_transition(&steps[0], &steps[0]), Ok(()));
    // A failed rollout of 2, removed again: the high-water mark keeps 2 retired.
    let rolled_back = ring("t", 1, 2, &[(1, 1)]);
    assert_eq!(validate_transition(&steps[1], &rolled_back), Ok(()));
    assert_eq!(
        validate_transition(&rolled_back, &ring("t", 1, 2, &[(1, 1), (2, 9)])),
        Err(TransitionError::VersionReused {
            version: 2,
            high_water_mark: 2
        })
    );
}

#[test]
fn refusals() {
    let old = ring("t", 1, 2, &[(1, 1), (2, 2)]);
    let cases = [
        (ring("u", 1, 2, &[(1, 1), (2, 2)]), TransitionError::Renamed),
        (
            ring("t", 1, 2, &[(1, 1), (2, 7)]),
            TransitionError::RootChanged { version: 2 },
        ),
        (
            ring("t", 1, 1, &[(1, 1)]),
            TransitionError::HighWaterMarkLowered { old: 2, new: 1 },
        ),
        (
            ring("t", 2, 2, &[(2, 2)]),
            TransitionError::CurrentRemoved { version: 1 },
        ),
        (
            ring("t", 3, 3, &[(1, 1), (2, 2), (3, 3)]),
            TransitionError::PromotedUnreleased { version: 3 },
        ),
    ];
    for (new, expected) in cases {
        assert_eq!(
            validate_transition(&old, &new),
            Err(expected.clone()),
            "{expected}"
        );
        assert!(!expected.to_string().contains("AQEB"));
    }
    let promoted = ring("t", 2, 2, &[(1, 1), (2, 2)]);
    assert_eq!(
        validate_transition(&promoted, &old),
        Err(TransitionError::PromotedBackwards { old: 2, new: 1 })
    );
    let gap = ring("t", 3, 3, &[(1, 1), (3, 3)]);
    assert_eq!(
        validate_transition(&gap, &ring("t", 3, 3, &[(1, 1), (2, 2), (3, 3)])),
        Err(TransitionError::VersionReused {
            version: 2,
            high_water_mark: 3
        })
    );
}
