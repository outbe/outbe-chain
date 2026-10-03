//! lifecycle regression scenarios.
use super::*;

#[test]
fn full_canonical_composition_cannot_skip_an_entire_later_nod_job() {
    use crate::snapshot::validation::{ocomp::verify_canonical_obligations, Incomplete};
    use std::cell::RefCell;
    for version in [1, 2] {
        for deleted in [false, true] {
            let fixtures = RefCell::new(None);
            super::super::with_canonical_frontiers(
                version,
                |layout| {
                    let first =
                        fixture(&layout.ocomp_root, 0x30, WorldwideDay::new(20_260_725), 10);
                    let second =
                        fixture(&layout.ocomp_root, 0x40, WorldwideDay::new(20_260_726), 10);
                    if deleted {
                        fs::remove_dir_all(
                            layout
                                .ocomp_root
                                .join("supervisor-v1/jobs")
                                .join(hex::encode(second.job_id)),
                        )
                        .unwrap();
                    }
                    *fixtures.borrow_mut() = Some((first, second));
                },
                |_| {
                    let fixtures = fixtures.borrow();
                    let (first, second) = fixtures.as_ref().unwrap();
                    canonical_owner(&[(first, 0), (second, 0)], None)
                },
                |state, source, layout, scratch| {
                    let result =
                        verify_canonical_obligations(state, source, layout, scratch, None, None);
                    if deleted {
                        let error = result
                            .err()
                            .expect("whole second canonical NOD job cannot pass");
                        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
                        assert!(format!("{error:#}").contains("NOD"), "{error:#}");
                    } else {
                        let audit = result.unwrap();
                        assert_eq!(audit.bounds.nod_entries, 2);
                        assert_eq!(audit.nod.jobs, 2);
                        assert_eq!(audit.nod.actions, 20);
                    }
                },
            );
        }
    }
}
