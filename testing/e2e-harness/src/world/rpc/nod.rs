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
        let ids = eth::read_call_result(
            &self.url(port),
            addresses::NOD_ADDR,
            &INod::tokensByOwnerCall { owner },
        )?;
        let [id] = ids[..] else {
            return Err(format!(
                "tokensByOwner returned {} Nods, expected exactly one",
                ids.len()
            ));
        };
        let nod_id = id.to_be_bytes::<32>().to_vec();
        let body = self.nod_data_on(port, &nod_id)?;
        Ok(Some((nod_id, body)))
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
