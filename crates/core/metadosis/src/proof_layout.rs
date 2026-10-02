use alloy_primitives::{b256, B256};

/// Fixed consensus slot used by OCM finality proof construction.
pub use crate::schema::OCOMP_JOB_RECORDS_BASE_SLOT;

/// Canonical fresh-devnet Metadosis layout description committed by genesis.
///
/// The corresponding schema test pins every value encoded here to the actual
/// generated storage layout. Changing either the layout or this description
/// therefore requires an explicit fresh-genesis contract revision.
pub const METADOSIS_STORAGE_LAYOUT_V1_CANONICAL: &[u8] = b"OUTBE_METADOSIS_STORAGE_LAYOUT_V1|worldwide_day_slots=10|closed_wwd_base_slot=13|ocomp_job_records_base_slot=19|league_snapshot_base_slot=28|terminal_receipt_base_slot=30|terminal_receipt_codec=OMTR1|live_index_codec=OMLI1|day_limit_receipt_base_slot=31|day_limit_receipt_slots=7";

pub const METADOSIS_STORAGE_LAYOUT_V1_HASH: B256 =
    b256!("b8cc7a9695ff2a15dffcf3d22598973f8c81e029c708889de155112449d12b95");
