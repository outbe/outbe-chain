//! Native Nod reads from EVM state.
use super::*;

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
        let result = eth::read_call_result(
            &self.url(port),
            addresses::NOD_ADDR,
            &INod::tokenOfOwnerByIndexCall {
                owner,
                index: U256::from(index),
            },
        )
        .map(|value| value.to_be_bytes::<32>().to_vec());
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

    pub fn nod_supply(&self, port: u16) -> Option<u64> {
        eth::read_call(
            &self.url(port),
            addresses::NOD_ADDR,
            &INod::totalSupplyCall {},
        )
        .and_then(|value| u64::try_from(value).ok())
    }
}

pub(in crate::world::rpc) fn classify_owner_index_result(
    index: u64,
    result: std::result::Result<Vec<u8>, String>,
) -> std::result::Result<Option<Vec<u8>>, String> {
    match result {
        Ok(nod_id) if nod_id.len() == outbe_primitives::wwd_entity_id::WwdEntityId::len_bytes() => {
            Ok(Some(nod_id))
        }
        Ok(nod_id) => Err(format!(
            "tokenOfOwnerByIndex returned a {}-byte NOD id",
            nod_id.len()
        )),
        Err(error) if index > 0 && error.to_ascii_lowercase().contains("index out of bounds") => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}
