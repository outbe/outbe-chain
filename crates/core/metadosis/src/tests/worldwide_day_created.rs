//! A creation count above zero means a Worldwide Day was created even when the
//! retained aggregate is empty. The membership half of the same predicate is
//! the legacy image: days already stored, and this slot still zero.

use super::*;

#[test]
fn a_creation_count_without_a_retained_day_still_counts_as_created() {
    with_storage(|storage| {
        assert!(!crate::api::has_created_worldwide_day(storage.clone()).unwrap());
        assert!(crate::api::worldwide_days(storage.clone())
            .unwrap()
            .is_empty());
        MetadosisContract::new(storage.clone())
            .worldwide_days_created
            .write(1)
            .unwrap();
        assert!(crate::api::worldwide_days(storage.clone())
            .unwrap()
            .is_empty());
        assert!(crate::api::has_created_worldwide_day(storage).unwrap());
    });
}
