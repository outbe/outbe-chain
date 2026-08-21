//! Auction bidders: funded accounts that commit and reveal one bid each.

use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{sol, SolValue};
use eyre::{eyre, Result};

use crate::internal::eth;
use crate::world::forge::DEPLOYER_KEY;

sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IMintableToken {
        function mint(address to, uint256 amount) external;
        function approve(address spender, uint256 amount) external returns (bool);
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IAuctionBids {
        function commitBid(uint32 worldwideDay, bytes32 commitHash) external;
        function revealBid(
            uint32 worldwideDay,
            uint16 quantity,
            uint32 bidRate,
            uint16 issuanceCurrency,
            uint16 referenceCurrency,
            uint64 chainId,
            bytes memory signature
        ) external;
    }
}

/// Native balance each bidder needs for its own gas, in whole COEN.
const BIDDER_NATIVE_COEN: u64 = 100;

/// The single-currency day both the issuance and reference sides carry.
const DAY_CURRENCY: u16 = 840;

#[derive(Clone, Debug)]
pub struct Bidder {
    pub key: String,
    pub address: Address,
    pub quantity: u16,
    pub bid_rate: u32,
}

pub fn derive(bids: &[(u16, u32)]) -> Result<Vec<Bidder>> {
    bids.iter()
        .enumerate()
        .map(|(index, (quantity, bid_rate))| {
            let seed = keccak256(format!("outbe-e2e:auction-bidder:{index}").as_bytes());
            let key = format!("0x{}", hex::encode(seed));
            let signer: PrivateKeySigner = key
                .parse()
                .map_err(|error| eyre!("derive bidder {index}: {error}"))?;
            Ok(Bidder {
                address: signer.address(),
                key,
                quantity: *quantity,
                bid_rate: *bid_rate,
            })
        })
        .collect()
}

pub fn fund(
    url: &str,
    token: Address,
    escrow: Address,
    bidders: &[Bidder],
    allowance: U256,
) -> Result<()> {
    for bidder in bidders {
        eth::send_value(
            url,
            bidder.address,
            DEPLOYER_KEY,
            eth::coen(BIDDER_NATIVE_COEN),
        )?;
        eth::send_call(
            url,
            token,
            DEPLOYER_KEY,
            &IMintableToken::mintCall {
                to: bidder.address,
                amount: allowance,
            },
            None,
        )?;
        eth::send_call(
            url,
            token,
            &bidder.key,
            &IMintableToken::approveCall {
                spender: escrow,
                amount: allowance,
            },
            None,
        )?;
    }
    Ok(())
}

fn bid_digest(auction: Address, chain_id: u64, worldwide_day: u32, bidder: &Bidder) -> B256 {
    let type_hash = keccak256(
        b"RevealBid(uint32 worldwideDay,address bidder,uint16 quantity,uint32 bidRate,uint16 issuanceCurrency,uint16 referenceCurrency)"
            .as_slice(),
    );
    let struct_hash = keccak256(
        (
            type_hash,
            worldwide_day,
            bidder.address,
            bidder.quantity,
            bidder.bid_rate,
            DAY_CURRENCY,
            DAY_CURRENCY,
        )
            .abi_encode(),
    );
    let domain_type_hash = keccak256(
        b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"
            .as_slice(),
    );
    let domain_separator = keccak256(
        (
            domain_type_hash,
            keccak256(b"IntexAuction".as_slice()),
            keccak256(b"1".as_slice()),
            U256::from(chain_id),
            auction,
        )
            .abi_encode(),
    );
    let mut preimage = Vec::with_capacity(66);
    preimage.extend_from_slice(&[0x19, 0x01]);
    preimage.extend_from_slice(domain_separator.as_slice());
    preimage.extend_from_slice(struct_hash.as_slice());
    keccak256(preimage)
}

/// Commit stores `keccak256(signature)` and reveal recovers the signer from that
/// same signature, so both stages share one.
fn bid_signature(
    auction: Address,
    chain_id: u64,
    worldwide_day: u32,
    bidder: &Bidder,
) -> Result<Vec<u8>> {
    let digest = bid_digest(auction, chain_id, worldwide_day, bidder);
    let signer: PrivateKeySigner = bidder
        .key
        .parse()
        .map_err(|error| eyre!("parse bidder key: {error}"))?;
    Ok(signer.sign_hash_sync(&digest)?.as_bytes().to_vec())
}

pub fn commit(
    url: &str,
    auction: Address,
    chain_id: u64,
    worldwide_day: u32,
    bidders: &[Bidder],
) -> Result<()> {
    for bidder in bidders {
        let signature = bid_signature(auction, chain_id, worldwide_day, bidder)?;
        eth::send_call(
            url,
            auction,
            &bidder.key,
            &IAuctionBids::commitBidCall {
                worldwideDay: worldwide_day,
                commitHash: keccak256(&signature),
            },
            None,
        )?;
    }
    Ok(())
}

pub fn reveal(
    url: &str,
    auction: Address,
    chain_id: u64,
    worldwide_day: u32,
    bidders: &[Bidder],
) -> Result<()> {
    for bidder in bidders {
        let signature = bid_signature(auction, chain_id, worldwide_day, bidder)?;
        eth::send_call(
            url,
            auction,
            &bidder.key,
            &IAuctionBids::revealBidCall {
                worldwideDay: worldwide_day,
                quantity: bidder.quantity,
                bidRate: bidder.bid_rate,
                issuanceCurrency: DAY_CURRENCY,
                referenceCurrency: DAY_CURRENCY,
                chainId: chain_id,
                signature: signature.into(),
            },
            None,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bid_signature_recovers_the_bidder_in_the_form_solidity_accepts() {
        let auction = Address::repeat_byte(0x11);
        let bidders = derive(&[(30, 800_000)]).expect("derive one bidder");
        let bidder = &bidders[0];

        let digest = bid_digest(auction, 424_242, 20_260_814, bidder);
        let signature = bid_signature(auction, 424_242, 20_260_814, bidder).expect("sign the bid");

        assert_eq!(signature.len(), 65);
        assert!(matches!(signature[64], 27 | 28), "v={}", signature[64]);

        let parsed = alloy_primitives::Signature::try_from(signature.as_slice())
            .expect("parse the signature back");
        assert_eq!(
            parsed
                .recover_address_from_prehash(&digest)
                .expect("recover the signer"),
            bidder.address
        );
    }
}
