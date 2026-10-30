//! Native Nod reads from EVM state.
use super::*;
use alloy_sol_types::SolError as _;

impl Rpc {
    pub fn assert_one_nod_for_owner(
        &self,
        port: u16,
        owner: Address,
        completion_block_number: u64,
    ) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let last_observation = match self.nod_for_owner(port, owner) {
                Ok(Some((nod_id, body))) => {
                    if body.owner != owner || body.nodId != U256::from_be_slice(&nod_id) {
                        return Err(eyre!("owner enumeration and getNod disagree"));
                    }
                    return Ok(());
                }
                Ok(None) => "balanceOf returned zero".to_owned(),
                Err(error) => error,
            };
            if Instant::now() >= deadline {
                return Err(eyre!(
                    "native Nod owner read did not become available: owner={owner:#x} completion_block={completion_block_number} head={:?} finalized={:?} last_observation={last_observation}",
                    self.head(port),
                    self.finalized(port),
                ));
            }
            sleep(Duration::from_millis(250));
        }
    }

    pub(crate) fn nod_for_owner(
        &self,
        port: u16,
        owner: Address,
    ) -> std::result::Result<Option<(Vec<u8>, crate::internal::eth::INod::NodData)>, String> {
        let balance = eth::read_call_result(
            &self.url(port),
            addresses::NOD_ADDR,
            &INod::balanceOfCall { owner },
        )?;
        if balance.is_zero() {
            return Ok(None);
        }
        if balance != U256::from(1) {
            return Err(format!(
                "balanceOf returned {balance}, expected exactly one"
            ));
        }
        let nod_id = self
            .nod_id_of_owner_by_index_on(port, owner, 0)?
            .ok_or_else(|| "index zero unexpectedly reported absence".to_owned())?;
        if self.nod_id_of_owner_by_index_on(port, owner, 1)?.is_some() {
            return Err("owner has more than one NOD".to_owned());
        }
        let body = self.nod_data_on(port, &nod_id)?;
        Ok(Some((nod_id, body)))
    }

    pub(crate) fn nod_id_of_owner_by_index_on(
        &self,
        port: u16,
        owner: Address,
        index: u64,
    ) -> std::result::Result<Option<Vec<u8>>, String> {
        let result = eth::read_call_with_revert(
            &self.url(port),
            addresses::NOD_ADDR,
            &INod::tokenOfOwnerByIndexCall {
                owner,
                index: U256::from(index),
            },
        )
        .map(|outcome| match outcome {
            eth::ViewCallOutcome::Returned(value) => {
                eth::ViewCallOutcome::Returned(value.to_be_bytes::<32>().to_vec())
            }
            eth::ViewCallOutcome::Reverted(data) => eth::ViewCallOutcome::Reverted(data),
        });
        classify_owner_index_result(index, result)
    }

    pub(crate) fn nod_data_on(
        &self,
        port: u16,
        nod_id: &[u8],
    ) -> std::result::Result<crate::internal::eth::INod::NodData, String> {
        eth::read_call_result(
            &self.url(port),
            addresses::NOD_ADDR,
            &INod::getNodCall {
                nodId: U256::from_be_slice(nod_id),
            },
        )
    }

    /// Nods `owners` hold together on the node at `port`.
    pub fn nods_of(&self, port: u16, owners: &[Address]) -> Option<u64> {
        owners
            .iter()
            .map(|owner| {
                eth::read_call(
                    &self.url(port),
                    addresses::NOD_ADDR,
                    &INod::balanceOfCall { owner: *owner },
                )
                .and_then(|value| u64::try_from(value).ok())
            })
            .sum()
    }
}

pub(in crate::world::rpc) fn classify_owner_index_result(
    index: u64,
    result: std::result::Result<eth::ViewCallOutcome<Vec<u8>>, String>,
) -> std::result::Result<Option<Vec<u8>>, String> {
    match result {
        Ok(eth::ViewCallOutcome::Returned(nod_id)) if nod_id.len() == 32 => Ok(Some(nod_id)),
        Ok(eth::ViewCallOutcome::Returned(nod_id)) => Err(format!(
            "tokenOfOwnerByIndex returned a {}-byte NOD id",
            nod_id.len()
        )),
        Ok(eth::ViewCallOutcome::Reverted(data))
            if index > 0 && data.as_ref() == INod::IndexOutOfBounds::SELECTOR.as_slice() =>
        {
            Ok(None)
        }
        Ok(eth::ViewCallOutcome::Reverted(data)) => Err(format!(
            "tokenOfOwnerByIndex reverted with {}",
            alloy_primitives::hex::encode_prefixed(data)
        )),
        Err(error) => Err(error),
    }
}
