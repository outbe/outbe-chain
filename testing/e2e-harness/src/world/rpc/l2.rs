use crate::world::rpc::*;

impl Rpc {
    /// Read one governed L2 registry entry.
    pub fn l2_network(&self, chain_id: u64) -> Option<(Address, Vec<u8>, bool)> {
        let network = eth::read_call(
            &self.cfg.rpc0,
            addresses::L2_REGISTRY_ADDR,
            &IL2Registry::getNetworkCall { chainId: chain_id },
        )?;
        Some((
            network.l1Address,
            network.publicKey.to_vec(),
            network.zkEnabled,
        ))
    }
}
