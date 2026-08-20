use super::*;

#[test]
fn issue_enrolls_in_floor_bin() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();
        let f = IntexFactoryContract::new(s.clone());
        let bin = IntexFactoryContract::price_to_bin(U256::from(EXPECTED_FLOOR)).unwrap();
        assert_eq!(
            f.unqualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, bin))
                .unwrap(),
            1
        );
    });
}

#[test]
fn insert_remove_unqualified_roundtrip() {
    with_factory(|s| {
        let mut f = IntexFactoryContract::new(s.clone());
        let floor = U256::from(2_000u64);
        let bin = IntexFactoryContract::price_to_bin(floor).unwrap();
        f.insert_unqualified(sid(11), REFERENCE_ISO, floor).unwrap();
        f.insert_unqualified(sid(22), REFERENCE_ISO, floor).unwrap();
        assert_eq!(
            f.unqualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, bin))
                .unwrap(),
            2
        );
        f.remove_unqualified_group(REFERENCE_ISO, WorldwideDay::new(11))
            .unwrap();
        assert_eq!(
            f.unqualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, bin))
                .unwrap(),
            1
        );
        f.remove_unqualified_group(REFERENCE_ISO, WorldwideDay::new(22))
            .unwrap();
        assert_eq!(
            f.unqualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, bin))
                .unwrap(),
            0
        );
    });
}

#[test]
fn try_qualify_gates_qualification_floor_and_latches() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();
        let mut f = IntexFactoryContract::new(s.clone());
        let floor = U256::from(EXPECTED_FLOOR);
        let immature = ISSUED_AT as u64 + 10;
        let mature = ISSUED_AT as u64 + 21 * DAY + 1;

        // Immature -> false.
        assert_eq!(
            qualify_day(
                &s,
                &mut f,
                7,
                QUALIFICATION_PERIOD,
                immature,
                floor + U256::from(1)
            ),
            0
        );
        // Mature but rate == floor (strict >) -> false.
        assert_eq!(
            qualify_day(&s, &mut f, 7, QUALIFICATION_PERIOD, mature, floor),
            0
        );
        // Mature + rate > floor -> qualifies, latched, removed from bin.
        assert_eq!(
            qualify_day(
                &s,
                &mut f,
                7,
                QUALIFICATION_PERIOD,
                mature,
                floor + U256::from(1)
            ),
            1
        );
        assert_eq!(
            outbe_intex::api::read_series(&s, sid(7))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Qualified
        );
        let bin = IntexFactoryContract::price_to_bin(floor).unwrap();
        assert_eq!(
            f.unqualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, bin))
                .unwrap(),
            0
        );
        // Already Qualified -> false.
        assert_eq!(
            qualify_day(
                &s,
                &mut f,
                7,
                QUALIFICATION_PERIOD,
                mature,
                floor + U256::from(1)
            ),
            0
        );
    });
}
