#[path = "../src/test_support/common.rs"]
mod test_support;

use outbe_ocomp::{cas, control, input_artifacts, input_ref_catalog};
#[path = "../src/test_support/snapshot.rs"]
mod snapshot_test_support;

#[path = "snapshot_readers/payout.rs"]
mod payout;

#[path = "snapshot_readers/receipt_listing.rs"]
mod receipt_listing;

#[path = "snapshot_readers/materialization.rs"]
mod materialization;

#[path = "snapshot_readers/admission.rs"]
mod admission;
#[path = "snapshot_readers/export_binding.rs"]
mod export_binding;

#[path = "snapshot_readers/filesystem.rs"]
mod filesystem;
