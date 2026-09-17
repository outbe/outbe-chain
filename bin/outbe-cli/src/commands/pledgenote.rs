//! Offline owner-side preparation. Only encrypted calldata goes to a relayer.
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{sol, SolCall};
use clap::Subcommand;
use eyre::{eyre, Result};
use outbe_primitives::{
    addresses::{CREDIS_FACTORY_ADDRESS, GRATIS_ADDRESS, GRATIS_FACTORY_ADDRESS},
    units::checked_protocol_to_native,
};
use outbe_tee::pledgenote::*;
use serde::{Deserialize, Serialize};
use std::{fs::OpenOptions, io::Write, path::PathBuf};

sol!("../../contracts/precompiles/src/IGratisFactory.sol");
sol!("../../contracts/precompiles/src/ICredisFactory.sol");
sol!("../../contracts/precompiles/src/IGratis.sol");

#[derive(Subcommand)]
pub enum PledgeNoteCmd {
    /// Read private JSON locally and write encrypted calldata for a relayer/CCA.
    Prepare {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Decrypt a query return or PledgeNoteCreated event into a private receipt.
    Decrypt {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Preparation {
    Owner {
        chain_id: u64,
        offer_public: B256,
        account: Address,
        modify_key: B256,
        nonce: u64,
        action: OwnerAction,
    },
    Use {
        chain_id: u64,
        offer_public: B256,
        owner_sa: Address,
        receipt: Box<Receipt>,
    },
}

#[derive(Serialize)]
struct Prepared {
    to: Address,
    data: Bytes,
    value: U256,
    read_only: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Decryption {
    view_key: B256,
    encrypted_receipt: Bytes,
}

fn prepare(input: Preparation) -> Result<Prepared> {
    match input {
        Preparation::Owner {
            chain_id,
            offer_public,
            account,
            modify_key,
            nonce,
            action,
        } => {
            let chain_id = B256::from(U256::from(chain_id));
            let key = zeroize::Zeroizing::new(modify_key.0);
            let mac = owner_mac(&key, chain_id, account, nonce, &action).map_err(|e| eyre!(e))?;
            let request = PrivateRequest::Owner {
                chain_id,
                account,
                nonce,
                action: action.clone(),
                mac,
            };
            let envelope = encrypt_request(offer_public.0, &request).map_err(|e| eyre!(e))?;
            let (to, data, read_only) = match action {
                OwnerAction::Create(quote) => {
                    let payload =
                        encode(&CreateRequest { quote, envelope }).map_err(|e| eyre!(e))?;
                    (
                        GRATIS_FACTORY_ADDRESS,
                        IGratisFactory::createPledgeNoteCall {
                            request: payload.into(),
                        }
                        .abi_encode(),
                        false,
                    )
                }
                OwnerAction::Cancel { .. } => (
                    GRATIS_FACTORY_ADDRESS,
                    IGratisFactory::cancelPledgeNoteCall {
                        encryptedAuth: envelope.into(),
                    }
                    .abi_encode(),
                    false,
                ),
                OwnerAction::Query | OwnerAction::QueryAt { .. } => (
                    GRATIS_ADDRESS,
                    IGratis::queryCall {
                        encryptedRequest: envelope.into(),
                    }
                    .abi_encode(),
                    true,
                ),
            };
            Ok(Prepared {
                to,
                data: data.into(),
                value: U256::ZERO,
                read_only,
            })
        }
        Preparation::Use {
            chain_id,
            offer_public,
            owner_sa,
            receipt,
        } => {
            let chain_id = B256::from(U256::from(chain_id));
            let terms = receipt
                .terms
                .ok_or_else(|| eyre!("receipt has no pledge quote"))?;
            let value = checked_protocol_to_native(terms.gratis_minor)
                .ok_or_else(|| eyre!("native stake overflow"))?;
            let authorization = use_mac(receipt.secret, chain_id, receipt.note_id, owner_sa)
                .map_err(|e| eyre!(e))?;
            let request = PrivateRequest::Use {
                chain_id,
                note_id: receipt.note_id,
                owner_sa,
                authorization,
            };
            let envelope = encrypt_request(offer_public.0, &request).map_err(|e| eyre!(e))?;
            Ok(Prepared {
                to: CREDIS_FACTORY_ADDRESS,
                data: ICredisFactory::issueCredisCall {
                    ownerSA: owner_sa,
                    encryptedUseAuth: envelope.into(),
                }
                .abi_encode()
                .into(),
                value,
                read_only: false,
            })
        }
    }
}

fn write_private(path: PathBuf, value: &impl Serialize) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.write_all(b"\n")?;
    Ok(())
}

impl PledgeNoteCmd {
    pub fn run(self) -> Result<()> {
        match self {
            Self::Prepare { input, output } => {
                let bytes = zeroize::Zeroizing::new(std::fs::read(input)?);
                write_private(output, &prepare(serde_json::from_slice(&bytes)?)?)
            }
            Self::Decrypt { input, output } => {
                let bytes = zeroize::Zeroizing::new(std::fs::read(input)?);
                let request: Decryption = serde_json::from_slice(&bytes)?;
                let key = zeroize::Zeroizing::new(request.view_key.0);
                let receipt =
                    decrypt_receipt(&key, &request.encrypted_receipt).map_err(|e| eyre!(e))?;
                write_private(output, &receipt)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_prepare_binds_quote_and_never_emits_owner_or_keys() {
        let account = Address::repeat_byte(0xa1);
        let modify_key = B256::repeat_byte(0xb2);
        let quote = Quote {
            asset: Address::repeat_byte(0xc3),
            principal_minor: U256::from(100),
            max_gratis_minor: U256::from(50),
            reference_currency: 840,
        };
        let prepared = prepare(Preparation::Owner {
            chain_id: 1,
            offer_public: B256::from([9u8; 32]),
            account,
            modify_key,
            nonce: 7,
            action: OwnerAction::Create(quote.clone()),
        })
        .unwrap();
        assert_eq!(prepared.to, GRATIS_FACTORY_ADDRESS);
        assert_eq!(prepared.value, U256::ZERO);
        assert!(!prepared.read_only);
        let call = IGratisFactory::createPledgeNoteCall::abi_decode(&prepared.data).unwrap();
        let request: CreateRequest = decode(&call.request).unwrap();
        assert_eq!(request.quote, quote);
        assert_eq!(
            request.envelope.len(),
            32 + 12 + ENVELOPE_PLAINTEXT_BYTES + 16
        );
        assert!(!prepared.data.windows(20).any(|w| w == account.as_slice()));
        assert!(!prepared
            .data
            .windows(32)
            .any(|w| w == modify_key.as_slice()));
    }
}
