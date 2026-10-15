//! The proceeds fan-in across several issuances of one day.

use super::*;

const DEADLINE: u64 = ISSUED_AT as u64 + 1000;

#[test]
fn arming_twice_in_one_day_expects_the_union_of_winning_chains() {
    with_registry(|storage| {
        api::arm_proceeds(&storage, DAY, &[10, 20], DEADLINE).unwrap();
        api::arm_proceeds(&storage, DAY, &[20, 30], DEADLINE).unwrap();

        // Both arm calls arm chain 20, and it must count once. Chain 30 joins the
        // two already armed, so the fan-in completes only on the third arrival.
        api::credit_proceeds(&storage, DAY, 10, U256::from(1u64)).unwrap();
        api::credit_proceeds(&storage, DAY, 20, U256::from(1u64)).unwrap();
        assert!(!api::proceeds_ready(&storage, DAY).unwrap());

        api::credit_proceeds(&storage, DAY, 30, U256::from(1u64)).unwrap();
        assert!(api::proceeds_ready(&storage, DAY).unwrap());
    });
}

#[test]
fn a_later_arming_does_not_complete_the_fan_in_early() {
    with_registry(|storage| {
        api::arm_proceeds(&storage, DAY, &[10, 20], DEADLINE).unwrap();
        api::credit_proceeds(&storage, DAY, 10, U256::from(1u64)).unwrap();

        // A second issuance arms one further chain while an earlier one is still
        // outstanding: the day must not read ready off the newcomer alone.
        api::arm_proceeds(&storage, DAY, &[30], DEADLINE).unwrap();
        api::credit_proceeds(&storage, DAY, 30, U256::from(1u64)).unwrap();
        assert!(!api::proceeds_ready(&storage, DAY).unwrap());

        api::credit_proceeds(&storage, DAY, 20, U256::from(1u64)).unwrap();
        assert!(api::proceeds_ready(&storage, DAY).unwrap());
    });
}
