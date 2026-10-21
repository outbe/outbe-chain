use crate::world::rpc::*;

impl Rpc {
    pub fn promis_limit_on(&self, port: u16) -> Option<U256> {
        eth::read_call(
            &self.url(port),
            addresses::PROMIS_LIMIT_ADDR,
            &IPromisLimit::promisLimitCall {},
        )
    }
}
