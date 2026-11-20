//! Witnessed Tribute calls. Consensus tags authenticate the precomputed inputs hash.

mod amounts;
mod call;
mod canary;
mod day;
mod keys;
mod nod;
mod offers;
mod schema;

pub use amounts::read_tribute_amounts;
pub use canary::{decrypt_canary_tribute_offer, open_canary_tribute_offer};
pub use day::{apply_day_operation, execute_lysis_fractions, read_day_amount, read_day_fractions};
pub use keys::{derive_tribute_keys, DeriveTributeKeysRequest, SealedTributeKeys};
pub use nod::{encrypt_nod_for_tribute, issue_nod_from_tribute};
pub use offers::{process_encrypted_offer_batch, process_offer_batch, process_offer_batch_with};
pub use schema::{
    EncryptedTributeOfferBatchV2, LysisFractionsExecutedV1, NodEncryptedForTributeV1,
    NodIssuedFromTributeV1, TributeAmountsReadV2, TributeDayAmountReadV2,
    TributeDayFractionsReadV1, TributeDayOpAppliedV2, TributeOfferBatch,
};
