use crate::world::rpc::*;

impl Rpc {
    pub fn desis_auction_stage_on(
        &self,
        port: u16,
        emission_day: u32,
        block_number: u64,
    ) -> Option<u8> {
        eth::read_call_at(
            &self.url(port),
            addresses::DESIS_ADDR,
            &IDesis::getAuctionStageCall {
                emissionDay: emission_day,
            },
            block_number,
        )
        .map(|stage| stage as u8)
    }
}
