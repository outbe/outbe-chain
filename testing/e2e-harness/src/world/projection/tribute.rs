use super::*;
use crate::internal::eth;
use crate::world::rpc::{FinalizedCheckpoint, Rpc};
use alloy_primitives::{Address, Bytes, B256};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::TRIBUTE_ADDRESS;
use outbe_tribute::precompile::ITribute;

pub(super) fn snapshot(
    rpc: &Rpc,
    port: u16,
    checkpoint: FinalizedCheckpoint,
    tx_hash: &str,
) -> Result<TributeProjectionSnapshot> {
    let receipt = eth::raw_json_result(
        &rpc.url(port),
        "eth_getTransactionReceipt",
        serde_json::json!([tx_hash]),
    )?;
    let logs = receipt["logs"]
        .as_array()
        .ok_or_else(|| eyre!("Tribute receipt omitted logs"))?;
    let mut issued = None;
    for log in logs {
        if log["address"]
            .as_str()
            .and_then(|address| address.parse::<Address>().ok())
            != Some(TRIBUTE_ADDRESS)
        {
            continue;
        }
        let topics = log["topics"]
            .as_array()
            .ok_or_else(|| eyre!("Tribute log omitted topics"))?
            .iter()
            .map(|topic| {
                topic
                    .as_str()
                    .ok_or_else(|| eyre!("invalid topic"))?
                    .parse::<B256>()
                    .map_err(Into::into)
            })
            .collect::<Result<Vec<_>>>()?;
        if topics.first() != Some(&ITribute::TributeIssued::SIGNATURE_HASH) {
            continue;
        }
        let data: Bytes = log["data"]
            .as_str()
            .ok_or_else(|| eyre!("Tribute log omitted data"))?
            .parse()?;
        let event = ITribute::TributeIssued::decode_raw_log(topics, &data)?;
        if issued.replace(event).is_some() {
            bail!("transaction emitted multiple Tribute identities");
        }
    }
    let issued = issued.ok_or_else(|| eyre!("transaction emitted no Tribute"))?;
    let id = WwdEntityId::from(issued.tributeId.to_be_bytes::<32>());
    let record = rpc
        .tribute_state_at(port, checkpoint, id)?
        .ok_or_else(|| eyre!("Tribute state is absent"))?;
    if record.owner != issued.owner || record.worldwide_day.value() != issued.worldwideDay {
        bail!("Tribute state differs from issuance event");
    }
    let (mut owner_ids, mut day_ids) =
        rpc.tribute_indexes_at(port, checkpoint, record.owner, record.worldwide_day)?;
    owner_ids.sort_unstable();
    day_ids.sort_unstable();
    for ids in [&owner_ids, &day_ids] {
        if ids.windows(2).any(|ids| ids[0] == ids[1]) || ids.binary_search(&id).is_err() {
            bail!("Tribute state index is incomplete or contains duplicates");
        }
    }
    Ok(TributeProjectionSnapshot {
        tribute: ProjectedTribute {
            raw_id: id,
            stored_body: record.stored_body()?.encode(),
        },
        owner_ids,
        day_ids,
    })
}
