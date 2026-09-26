use alloy_primitives::{b256, B256};

/// Fixed consensus slot used by OCM finality proof construction.
pub use crate::schema::OCOMP_JOB_RECORDS_BASE_SLOT;

/// Canonical fresh-devnet Metadosis layout description committed by genesis.
///
/// The corresponding schema test pins every value encoded here to the actual
/// generated storage layout. Changing either the layout or this description
/// therefore requires an explicit fresh-genesis contract revision.
pub const METADOSIS_STORAGE_LAYOUT_V1_CANONICAL: &[u8] = b"OUTBE_METADOSIS_STORAGE_LAYOUT_V1|worldwide_day_slots=10|active_wwd_count_slot=11|closed_wwd_base_slot=14|terminal_receipt_base_slot=31|terminal_receipt_slots=6|capacity_forfeiture_base_slot=37|capacity_forfeiture_slots=13|day_limit_receipt_base_slot=50|day_limit_receipt_slots=7";

pub const METADOSIS_STORAGE_LAYOUT_V1_HASH: B256 =
    b256!("193b70d52eaf69583d3407af7281cbff732334fb32992ee0be69404a841c468a");
