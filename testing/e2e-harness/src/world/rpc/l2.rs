use crate::world::rpc::*;

impl Rpc {
    /// Chain id L2Registry holds for `l1_address` (`0` when unregistered).
    ///
    /// This identifies the registry administrator. Tribute admission selects
    /// the network by its explicit chain id; the offer caller may be unrelated.
    pub fn l2_chain_by_l1_address(&self, l1_address: Address) -> Option<u64> {
        eth::read_call(
            &self.cfg.rpc0,
            addresses::L2_REGISTRY_ADDR,
            &IL2Registry::chainIdByL1AddressCall {
                l1Address: l1_address,
            },
        )
    }

    /// Read one governed L2 registry entry.
    pub fn l2_network(&self, chain_id: u64) -> Option<(Address, Vec<u8>)> {
        let network = eth::read_call(
            &self.cfg.rpc0,
            addresses::L2_REGISTRY_ADDR,
            &IL2Registry::getNetworkCall { chainId: chain_id },
        )?;
        Some((network.l1Address, network.publicKey.to_vec()))
    }
}
