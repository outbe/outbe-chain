//! Offer commands.

use alloy_primitives::{Bytes, U256};
use alloy_sol_types::SolCall;
use clap::{Args, Subcommand};
use eyre::Result;
use outbe_system_common::time::WorldwideDay;
use outbe_zk::{noir::L2_CIRCUITS_REGISTRY, L2CircuitVersion};

use crate::abi::{
    INodFactory, ITeeRegistry, IWorldwideDay, NOD_FACTORY_ADDR, TEE_REGISTRY_ADDR,
    WORLDWIDE_DAY_ADDR,
};
use crate::errors::OfferError;
use crate::rpc::Rpc;

type WorldwideDayTotalsReturn = <IWorldwideDay::worldwideDayTotalsCall as SolCall>::Return;

fn canonical_amount_base(value: &str) -> std::result::Result<String, OfferError> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| OfferError::InvalidAmountBase)?;
    if parsed.to_string() != value {
        return Err(OfferError::InvalidAmountBase);
    }
    Ok(value.to_owned())
}

fn canonical_amount_micro(value: &str) -> std::result::Result<String, OfferError> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| OfferError::InvalidAmountMicro)?;
    if parsed.to_string() != value || parsed >= 1_000_000 {
        return Err(OfferError::InvalidAmountMicro);
    }
    Ok(value.to_owned())
}

#[derive(Subcommand)]
pub enum OfferCmd {
    /// Show the Nod totals of the Worldwide Day of an emission day
    WorldwideDayTotals {
        /// Emission day, `YYYYMMDD`
        emission_day: u32,
    },
    /// Submit an encrypted offer and issue its Nod (decrypted inside the enclave).
    /// Encrypts to the DKG-derived offer key registered in the TeeRegistry and
    /// sends `issueNod`. Requires `--private-key` and the ZK offer inputs
    /// (`--zk-proof`, `--zk-merkle-root`, `--signature`).
    IssueNod(Box<IssueNodArgs>),
}

#[derive(Args)]
pub struct IssueNodArgs {
    /// WorldwideDay (must be in OFFERING status), e.g. 20241220
    worldwide_day: WorldwideDay,
    /// Issuance amount in whole units (`amount_base`)
    #[arg(long, default_value = "100", value_parser = canonical_amount_base)]
    amount: String,
    /// Six-decimal raw remainder (`amount_micro`, 0..999999)
    #[arg(long, default_value = "0", value_parser = canonical_amount_micro)]
    amount_micro: String,
    /// Issuance currency, ISO 4217 numeric (e.g. 840 = USD)
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=999))]
    issuance_currency: u16,
    /// Reference currency the rights are priced in, ISO 4217 numeric
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=999))]
    reference_currency: u16,
    /// L2 zkMerkleRoot bytes (`0x`-hex).
    #[arg(long)]
    zk_merkle_root: String,
    /// Combined offer proof bytes (`0x`-hex), including its four public
    /// inputs. Verifies under the circuit version enabled for `--l2-chain-id`.
    #[arg(long, value_parser = parse_zk_proof)]
    zk_proof: Bytes,
    /// Registered L2 chain id selecting the offer circuit. Required.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    l2_chain_id: u32,
    /// Circuit version. Defaults to the latest enabled version for the selected L2.
    #[arg(
        long,
        required = false,
        default_value_ifs = circuit_version_defaults(),
        value_parser = clap::builder::NonEmptyStringValueParser::new(),
    )]
    circuit_version: String,
    /// Exact 32-byte OfferDraft id (`0x`-hex) used to construct
    /// `nft_hash` and `binding_hash`.
    #[arg(long)]
    tribute_draft_id: String,
    /// Exact 32-byte SpendingUnit hash (`0x`-hex) included in the OfferDraft.
    #[arg(long)]
    su_hash: String,
    /// BLS MinSig signature (compressed G1, 48 bytes, `0x`-hex) over `--zk-merkle-root`
    /// produced with the network key registered in the L2Registry.
    #[arg(long)]
    signature: String,
}

impl OfferCmd {
    pub async fn run(self, client: &(impl Rpc + Sync), private_key: Option<&str>) -> Result<()> {
        match self {
            Self::WorldwideDayTotals { emission_day } => {
                worldwide_day_totals(client, emission_day).await
            }
            Self::IssueNod(args) => issue_nod(client, private_key, *args).await,
        }
    }
}

async fn fetch_worldwide_day_totals(
    client: &(impl Rpc + Sync),
    emission_day: u32,
) -> Result<WorldwideDayTotalsReturn> {
    let call = IWorldwideDay::worldwideDayTotalsCall {
        emissionDay: emission_day,
    };
    let result = client
        .eth_call(WORLDWIDE_DAY_ADDR, &call.abi_encode())
        .await?;
    Ok(IWorldwideDay::worldwideDayTotalsCall::abi_decode_returns(
        &result,
    )?)
}

async fn worldwide_day_totals(client: &(impl Rpc + Sync), emission_day: u32) -> Result<()> {
    let ret = fetch_worldwide_day_totals(client, emission_day).await?;

    println!("Emission day:           {}", emission_day);
    println!("Nod Count:              {}", ret.nodCount);
    println!("Encrypted Nominal:      {}", ret.encryptedNominal);
    println!("Encrypted Load:         {}", ret.encryptedLoad);
    println!("Sealed:                 {}", ret.isSealed);
    Ok(())
}

/// Submit an encrypted offer. This function:
/// 1. Reads the DKG-derived offer key from the TeeRegistry.
/// 2. Encrypts the payload to it (X25519 ECDHE + HKDF-SHA256 + ChaCha20Poly1305,
///    byte-identical to the enclave decrypt path).
/// 3. Sends `issueNod`.
///
/// The enclave decrypts the payload during execution, and the Nod factory issues the Nod.
#[allow(clippy::too_many_arguments)]
async fn issue_nod(
    client: &(impl Rpc + Sync),
    private_key: Option<&str>,
    args: IssueNodArgs,
) -> Result<()> {
    let IssueNodArgs {
        worldwide_day,
        amount: amount_base,
        amount_micro,
        issuance_currency,
        reference_currency,
        zk_merkle_root,
        zk_proof,
        l2_chain_id,
        circuit_version,
        tribute_draft_id,
        su_hash,
        signature,
    } = args;
    let signer = crate::commands::require_signer(private_key)?;
    let creator = signer.address();
    let zk_merkle_root = decode_hex_bytes(&zk_merkle_root, "--zk-merkle-root")?;
    let signature = decode_hex_bytes(&signature, "--signature")?;
    let tribute_draft_id = offer_hex32(&tribute_draft_id, "--tribute-draft-id")?;
    let su_hash = offer_hex32(&su_hash, "--su-hash")?;

    // 1. Read the DKG-derived offer public key from the TeeRegistry (0xEE0A).
    let bootstrapped = {
        let r = client
            .eth_call(
                TEE_REGISTRY_ADDR,
                &ITeeRegistry::isBootstrappedCall {}.abi_encode(),
            )
            .await?;
        ITeeRegistry::isBootstrappedCall::abi_decode_returns(&r)?
    };
    if !bootstrapped {
        return Err(OfferError::TeeRegistryNotBootstrapped.into());
    }
    let offer_pub_u256 = {
        let r = client
            .eth_call(
                TEE_REGISTRY_ADDR,
                &ITeeRegistry::tributeOfferPublicKeyCall {}.abi_encode(),
            )
            .await?;
        ITeeRegistry::tributeOfferPublicKeyCall::abi_decode_returns(&r)?
    };
    let offer_pub: [u8; 32] = offer_pub_u256.to_be_bytes();
    println!("offer key (DKG-derived): 0x{}", hex::encode(offer_pub));

    // 2. Build the plaintext payload matching the proof's private inputs.
    let wwd: u32 = worldwide_day.into();
    // worldwide_day + currency are cleartext ABI args (below) so the node can
    // admit and price the offer without decrypting. The ciphertext carries only
    // what must stay confidential. `referenceCurrency` is a separate axis again
    // (it denominates the rights' entry and call prices), not the pricing key.
    let payload = serde_json::json!({
        "creator": format!("{creator:?}"),
        "tribute_draft_id": tribute_draft_id,
        "amount_base": amount_base,
        "amount_micro": amount_micro,
        "su_hashes": [su_hash],
        "wallet_addresses": [],
        "sra_addresses": [],
    });
    let plaintext = serde_json::to_vec(&payload)?;

    // 3. Encrypt to the offer key via the shared recipe (protocol
    //    `OFFER_HKDF_SALT` + ChaCha20Poly1305), byte-compatible with the enclave
    //    decrypt path and the node's canary probe.
    let (cipher_text, nonce, eph_pub) =
        outbe_core_common::confidential_tee::encrypt_offer(&offer_pub, &plaintext)
            .map_err(OfferError::OfferEncryptionFailed)?;

    // 4. Build + send `issueNod` (msg.value MUST be 0).
    let call = INodFactory::issueNodCall {
        cipherText: cipher_text.into(),
        nonce: nonce.to_vec().into(),
        ephemeralPubkey: U256::from_be_bytes(eph_pub),
        worldwideDay: wwd,
        issuanceCurrency: issuance_currency,
        referenceCurrency: reference_currency,
        zkProof: zk_proof,
        chainId: l2_chain_id,
        version: circuit_version,
        zkMerkleRoot: zk_merkle_root,
        signature,
    };
    // `eth_estimateGas` cannot faithfully simulate the in-enclave decrypt, so
    // send with an explicit gas limit.
    let tx_hash = signer
        .send_tx_with_gas(
            client,
            NOD_FACTORY_ADDR,
            call.abi_encode(),
            U256::ZERO,
            8_000_000,
        )
        .await?;

    println!("issueNod tx: {tx_hash}");
    println!(
        "  creator={creator:?} worldwide_day={wwd} issuance_currency={issuance_currency} reference_currency={reference_currency} amount_base={amount_base} amount_micro={amount_micro}"
    );
    println!(
        "  l2_chain_id={l2_chain_id} circuit_version={:?}",
        call.version
    );
    Ok(())
}

/// Decode a hex CLI argument into raw bytes.
fn decode_hex_bytes(value: &str, flag: &str) -> Result<Bytes> {
    let stripped = value.strip_prefix("0x").unwrap_or(value);
    if stripped.is_empty() {
        return Ok(Bytes::new());
    }
    let bytes = hex::decode(stripped).map_err(|source| OfferError::InvalidHexFlag {
        flag: flag.to_owned(),
        source,
    })?;
    Ok(Bytes::from(bytes))
}

fn parse_zk_proof(value: &str) -> Result<Bytes> {
    let proof = decode_hex_bytes(value, "--zk-proof")?;
    if proof.is_empty() {
        return Err(OfferError::EmptyZkProof.into());
    }
    Ok(proof)
}

fn latest_circuit_version(enabled: &[L2CircuitVersion]) -> Option<&'static str> {
    enabled
        .iter()
        .max_by_key(|entry| {
            let mut parts = entry.version.split('.').map(|part| {
                part.parse::<u64>()
                    .expect("canonical circuit version must be major.minor.patch")
            });
            (parts.next(), parts.next(), parts.next())
        })
        .map(|entry| entry.version)
}

fn circuit_version_defaults() -> impl Iterator<
    Item = (
        &'static str,
        clap::builder::ArgPredicate,
        Option<&'static str>,
    ),
> {
    use clap::builder::ArgPredicate;
    L2_CIRCUITS_REGISTRY.iter().map(|chain| {
        (
            "l2_chain_id",
            ArgPredicate::Equals(chain.chain_id.to_string().into()),
            latest_circuit_version(chain.circuits),
        )
    })
}

fn offer_hex32(value: &str, flag: &str) -> Result<String> {
    let bytes = decode_hex_bytes(value, flag)?;
    if bytes.len() != 32 {
        return Err(OfferError::InvalidHex32Length {
            flag: flag.to_owned(),
            actual: bytes.len(),
        }
        .into());
    }
    Ok(format!("0x{}", hex::encode(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::mock::{call_map, MockRpc};
    use clap::Parser;
    use std::collections::HashMap;

    #[derive(Parser)]
    struct OfferHarness {
        #[command(subcommand)]
        command: OfferCmd,
    }

    #[derive(serde::Deserialize)]
    struct CanonicalBaseCases {
        accepted_base: Vec<String>,
        rejected_base: Vec<String>,
        accepted_micro: Vec<String>,
        rejected_micro: Vec<String>,
    }

    fn canonical_base_cases() -> CanonicalBaseCases {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../testdata/offer/canonical-amounts-v1.json"
        )))
        .unwrap()
    }

    fn worldwide_day_totals_mock() -> MockRpc {
        let worldwide_day_totals = (
            2u32,
            Bytes::from(vec![0x53; 56]),
            Bytes::from(vec![0x54; 56]),
            true,
        )
            .into();
        let mut map = HashMap::new();
        map.insert(
            (
                WORLDWIDE_DAY_ADDR,
                IWorldwideDay::worldwideDayTotalsCall::SELECTOR,
            ),
            IWorldwideDay::worldwideDayTotalsCall::abi_encode_returns(&worldwide_day_totals),
        );
        MockRpc {
            eth_call_map: Some(call_map(map)),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn test_fetch_worldwide_day_totals_returns_expected_values() {
        let mock = worldwide_day_totals_mock();

        let result = fetch_worldwide_day_totals(&mock, 20241220).await.unwrap();
        assert_eq!(result.nodCount, 2);
        assert_eq!(result.encryptedNominal, Bytes::from(vec![0x53; 56]));
        assert_eq!(result.encryptedLoad, Bytes::from(vec![0x54; 56]));
        assert!(result.isSealed);
    }

    #[test]
    fn explicit_offer_input_must_be_exactly_32_bytes() {
        assert!(offer_hex32("0x0102", "--su-hash").is_err());

        let value = format!("0x{}", "01".repeat(32));
        assert_eq!(offer_hex32(&value, "--su-hash").unwrap(), value);
    }

    /// Required proof inputs, in `flag, value` pairs; the L2 is selected below.
    const REQUIRED_OFFER_FLAGS: [&str; 14] = [
        "--issuance-currency",
        "949",
        "--reference-currency",
        "840",
        "--zk-proof",
        "0x01",
        "--zk-merkle-root",
        "0x01",
        "--signature",
        "0x01",
        "--tribute-draft-id",
        "0x1111111111111111111111111111111111111111111111111111111111111111",
        "--su-hash",
        "0x2222222222222222222222222222222222222222222222222222222222222222",
    ];

    /// `offer issue-nod 20250115` plus `extra` plus the required ZK flags.
    fn offer_argv(extra: &[&str]) -> Vec<String> {
        let mut argv: Vec<String> = Vec::new();
        argv.extend(["offer", "issue-nod", "20250115"].map(str::to_owned));
        argv.extend(extra.iter().map(|value| (*value).to_owned()));
        argv.extend(REQUIRED_OFFER_FLAGS.iter().map(|value| (*value).to_owned()));
        if !extra.contains(&"--l2-chain-id") {
            argv.extend(["--l2-chain-id", "57005"].map(str::to_owned));
        }
        argv
    }

    #[test]
    fn offer_cli_requires_the_zk_offer_inputs() {
        for missing in [
            "--issuance-currency",
            "--reference-currency",
            "--zk-proof",
            "--zk-merkle-root",
            "--signature",
            "--tribute-draft-id",
            "--su-hash",
            "--l2-chain-id",
        ] {
            let mut argv = offer_argv(&[]);
            let index = argv
                .iter()
                .position(|arg| arg == missing)
                .unwrap_or_else(|| panic!("{missing} is not in the offered argv"));
            argv.drain(index..=index + 1);
            assert!(
                OfferHarness::try_parse_from(argv).is_err(),
                "{missing} must be required"
            );
        }
        assert!(OfferHarness::try_parse_from(offer_argv(&[])).is_ok());
    }

    #[test]
    fn offer_cli_keeps_the_issuance_and_reference_currencies_apart() {
        let parsed = OfferHarness::try_parse_from(offer_argv(&[])).unwrap();
        let OfferCmd::IssueNod(args) = parsed.command else {
            panic!("expected offer command");
        };
        let IssueNodArgs {
            issuance_currency,
            reference_currency,
            ..
        } = *args;
        assert_eq!((issuance_currency, reference_currency), (949, 840));

        for code in ["0", "1000"] {
            let mut argv = offer_argv(&[]);
            let index = argv
                .iter()
                .position(|arg| arg == "--issuance-currency")
                .unwrap();
            argv[index + 1] = code.to_owned();
            assert!(
                OfferHarness::try_parse_from(argv).is_err(),
                "{code} is not an ISO 4217 code"
            );
        }
    }

    #[test]
    fn offer_cli_defaults_to_latest_version_of_the_explicit_l2() {
        for (extra, expected_chain, expected_version) in [
            (vec![], 57_005, "1.1.0"),
            (vec!["--l2-chain-id", "9900501"], 9_900_501, "1.0.0"),
            (
                vec!["--l2-chain-id", "7", "--circuit-version", "9.9.9"],
                7,
                "9.9.9",
            ),
        ] {
            let parsed = OfferHarness::try_parse_from(offer_argv(&extra)).unwrap();
            let OfferCmd::IssueNod(args) = parsed.command else {
                panic!("expected offer command");
            };
            let IssueNodArgs {
                l2_chain_id,
                circuit_version,
                ..
            } = *args;
            assert_eq!(l2_chain_id, expected_chain);
            assert_eq!(circuit_version, expected_version);
        }
        assert!(OfferHarness::try_parse_from(offer_argv(&["--l2-chain-id", "7"])).is_err());
    }

    #[test]
    fn latest_enabled_version_uses_numeric_semver_order() {
        let base = L2_CIRCUITS_REGISTRY[0].circuits[0];
        let versions =
            ["1.10.0", "1.2.0", "1.9.0"].map(|version| L2CircuitVersion { version, ..base });
        assert_eq!(latest_circuit_version(&versions), Some("1.10.0"));
        assert_eq!(latest_circuit_version(&[]), None);
    }

    #[test]
    fn offer_cli_rejects_empty_proofs_and_selectors() {
        for proof in ["", "0x", "not-hex"] {
            let mut argv = offer_argv(&[]);
            let index = argv.iter().position(|arg| arg == "--zk-proof").unwrap();
            argv[index + 1] = proof.to_owned();
            assert!(OfferHarness::try_parse_from(argv).is_err());
        }
        assert!(OfferHarness::try_parse_from(offer_argv(&["--l2-chain-id", "0"])).is_err());
        assert!(OfferHarness::try_parse_from(offer_argv(&["--circuit-version", ""])).is_err());
    }

    #[test]
    fn offer_cli_accepts_only_canonical_unsigned_whole_base_amounts() {
        let cases = canonical_base_cases();
        for canonical in cases.accepted_base {
            assert!(OfferHarness::try_parse_from(offer_argv(&["--amount", &canonical])).is_ok());
        }

        for noncanonical in cases.rejected_base {
            assert!(
                OfferHarness::try_parse_from(offer_argv(&["--amount", &noncanonical])).is_err(),
                "non-canonical amount_base {noncanonical:?} was accepted"
            );
        }
    }

    #[test]
    fn offer_cli_accepts_only_canonical_six_decimal_micro_remainders() {
        let cases = canonical_base_cases();
        for canonical in cases.accepted_micro {
            assert!(
                OfferHarness::try_parse_from(offer_argv(&["--amount-micro", &canonical])).is_ok()
            );
        }

        for noncanonical in cases.rejected_micro {
            assert!(
                OfferHarness::try_parse_from(offer_argv(&["--amount-micro", &noncanonical]))
                    .is_err(),
                "non-canonical amount_micro {noncanonical:?} was accepted"
            );
        }
    }

    #[test]
    fn offer_cli_rejects_legacy_amount_atto_flag() {
        assert!(OfferHarness::try_parse_from(offer_argv(&["--amount-atto", "0"])).is_err());
    }
}
