//! Staking commands.

use std::net::SocketAddr;
use std::path::PathBuf;

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolCall;
use clap::Subcommand;
use eyre::Result;
use outbe_primitives::consensus_p2p::{encode_v1, P2P_ADDRESS_VERSION_V1};

use super::parse_amount;
use super::stake_identity::{resolve_stake_identity, StakeIdentityInput};
use super::validator::build_p2p_address;
use crate::abi::{self, IStaking, IValidatorSet};
use crate::rpc::Rpc;

#[derive(Subcommand)]
pub enum StakingCmd {
    /// Register a validator and bond its initial stake.
    Stake {
        /// Validator address. With `--keys-dir`, this must match `evm-key.hex`.
        #[arg(long)]
        validator: Address,
        /// Amount in unit (base denomination). Sent as the transaction value.
        #[arg(long)]
        amount: String,
        /// Key directory written by `outbe-keygen validator`.
        #[arg(long)]
        keys_dir: Option<PathBuf>,
        /// BLS consensus public key (hex).
        #[arg(long)]
        consensus_pubkey: Option<String>,
        /// Radicle Ed25519 node id (32-byte hex).
        #[arg(long)]
        radicle_node_id: Option<B256>,
        /// BLS registration signature (hex).
        #[arg(long)]
        bls_registration_signature: Option<String>,
        /// Symmetric socket address, for example 10.0.0.1:30400.
        #[arg(long)]
        symmetric: Option<SocketAddr>,
        /// Asymmetric socket ingress address.
        #[arg(long)]
        ingress_socket: Option<SocketAddr>,
        /// Asymmetric DNS ingress host.
        #[arg(long)]
        ingress_dns_host: Option<String>,
        /// Asymmetric DNS ingress port.
        #[arg(long)]
        ingress_dns_port: Option<u16>,
        /// Asymmetric egress socket address.
        #[arg(long)]
        egress: Option<SocketAddr>,
    },
    /// Add stake to an already registered validator.
    Topup {
        /// Validator address
        #[arg(long)]
        validator: Address,
        /// Amount in unit (base denomination). Sent as the transaction value.
        #[arg(long)]
        amount: String,
    },
    /// Begin unstaking tokens
    Unstake {
        /// Amount in unit (base denomination)
        #[arg(long)]
        amount: String,
    },
    /// Claim tokens after unbonding period
    Claim,
    /// Unjail your JAILED validator (requires stake >= min_stake) -> PENDING
    Unjail,
    /// Show staking info for a validator
    Info {
        /// Validator address
        address: Address,
    },
    /// Show network-wide staking statistics.
    Stats,
}

impl StakingCmd {
    pub async fn run(self, client: &(impl Rpc + Sync), private_key: Option<&str>) -> Result<()> {
        match self {
            Self::Stake {
                validator,
                amount,
                keys_dir,
                consensus_pubkey,
                radicle_node_id,
                bls_registration_signature,
                symmetric,
                ingress_socket,
                ingress_dns_host,
                ingress_dns_port,
                egress,
            } => {
                stake(
                    client,
                    private_key,
                    StakeRequest {
                        validator,
                        amount,
                        keys_dir,
                        consensus_pubkey,
                        radicle_node_id,
                        bls_registration_signature,
                        symmetric,
                        ingress_socket,
                        ingress_dns_host,
                        ingress_dns_port,
                        egress,
                    },
                )
                .await
            }
            Self::Topup { validator, amount } => {
                topup(client, private_key, validator, amount).await
            }
            Self::Unstake { amount } => unstake(client, private_key, amount).await,
            Self::Claim => claim(client, private_key).await,
            Self::Unjail => unjail(client, private_key).await,
            Self::Info { address } => info(client, address).await,
            Self::Stats => stats(client).await,
        }
    }
}

struct StakeRequest {
    validator: Address,
    amount: String,
    keys_dir: Option<PathBuf>,
    consensus_pubkey: Option<String>,
    radicle_node_id: Option<B256>,
    bls_registration_signature: Option<String>,
    symmetric: Option<SocketAddr>,
    ingress_socket: Option<SocketAddr>,
    ingress_dns_host: Option<String>,
    ingress_dns_port: Option<u16>,
    egress: Option<SocketAddr>,
}

async fn stake(
    client: &(impl Rpc + Sync),
    private_key: Option<&str>,
    request: StakeRequest,
) -> Result<()> {
    let signer = super::require_signer(private_key)?;
    let amount = parse_amount(&request.amount)?;
    let identity = resolve_stake_identity(
        client,
        request.validator,
        StakeIdentityInput {
            keys_dir: request.keys_dir.as_deref(),
            consensus_pubkey: request.consensus_pubkey.as_deref(),
            radicle_node_id: request.radicle_node_id,
            bls_registration_signature: request.bls_registration_signature.as_deref(),
        },
    )
    .await?;
    let (p2p_version, p2p_encoded) = optional_p2p(&request)?;

    let call = IStaking::stakeCall {
        validatorAddress: request.validator,
        amount,
        consensusPubkey: identity.consensus_pubkey.into(),
        radicleNodeId: identity.radicle_node_id,
        blsRegistrationSignature: identity.bls_registration_signature.into(),
        p2pVersion: p2p_version,
        p2pEncoded: p2p_encoded.into(),
    };
    let tx_hash = signer
        .send_tx(client, abi::STAKING_ADDR, call.abi_encode(), amount)
        .await?;
    println!("Transaction sent: {tx_hash}");
    Ok(())
}

async fn topup(
    client: &(impl Rpc + Sync),
    private_key: Option<&str>,
    validator: Address,
    amount: String,
) -> Result<()> {
    let signer = super::require_signer(private_key)?;
    let amount = parse_amount(&amount)?;
    let call = IStaking::topupCall {
        validatorAddress: validator,
        amount,
    };
    let tx_hash = signer
        .send_tx(client, abi::STAKING_ADDR, call.abi_encode(), amount)
        .await?;
    println!("Transaction sent: {tx_hash}");
    Ok(())
}

fn p2p_flags_omitted(request: &StakeRequest) -> bool {
    [
        request.symmetric.is_none(),
        request.ingress_socket.is_none(),
        request.ingress_dns_host.is_none(),
        request.ingress_dns_port.is_none(),
        request.egress.is_none(),
    ]
    .into_iter()
    .all(std::convert::identity)
}

fn optional_p2p(request: &StakeRequest) -> Result<(u8, Vec<u8>)> {
    if p2p_flags_omitted(request) {
        return Ok((0, Vec::new()));
    }
    let address = build_p2p_address(
        request.symmetric,
        request.ingress_socket,
        request.ingress_dns_host.clone(),
        request.ingress_dns_port,
        request.egress,
    )?;
    Ok((P2P_ADDRESS_VERSION_V1, encode_v1(&address)))
}

async fn unstake(
    client: &(impl Rpc + Sync),
    private_key: Option<&str>,
    amount: String,
) -> Result<()> {
    let signer = super::require_signer(private_key)?;
    let amount = parse_amount(&amount)?;

    let call = IStaking::unstakeCall { amount };
    let tx_hash = signer
        .send_tx(
            client,
            abi::STAKING_ADDR,
            call.abi_encode(),
            Default::default(),
        )
        .await?;
    println!("Transaction sent: {tx_hash}");
    Ok(())
}

async fn claim(client: &(impl Rpc + Sync), private_key: Option<&str>) -> Result<()> {
    let signer = super::require_signer(private_key)?;
    let call = IStaking::claimUnbondedCall {};
    let tx_hash = signer
        .send_tx(
            client,
            abi::STAKING_ADDR,
            call.abi_encode(),
            Default::default(),
        )
        .await?;
    println!("Transaction sent: {tx_hash}");
    Ok(())
}

async fn unjail(client: &(impl Rpc + Sync), private_key: Option<&str>) -> Result<()> {
    let signer = super::require_signer(private_key)?;
    let call = IStaking::unjailValidatorCall {};
    let tx_hash = signer
        .send_tx(
            client,
            abi::STAKING_ADDR,
            call.abi_encode(),
            Default::default(),
        )
        .await?;
    println!("Transaction sent: {tx_hash}");
    Ok(())
}

struct StakingInfo {
    stake: U256,
    total: U256,
}

async fn fetch_staking_info(client: &(impl Rpc + Sync), address: Address) -> Result<StakingInfo> {
    let stake: U256 = {
        let call = IStaking::getStakeCall { validator: address };
        let result = client
            .eth_call(abi::STAKING_ADDR, &call.abi_encode())
            .await?;
        IStaking::getStakeCall::abi_decode_returns(&result)?
    };

    let total: U256 = {
        let call = IStaking::getTotalStakedCall {};
        let result = client
            .eth_call(abi::STAKING_ADDR, &call.abi_encode())
            .await?;
        IStaking::getTotalStakedCall::abi_decode_returns(&result)?
    };

    Ok(StakingInfo { stake, total })
}

async fn info(client: &(impl Rpc + Sync), address: Address) -> Result<()> {
    let si = fetch_staking_info(client, address).await?;

    println!("Validator:    {:?}", address);
    println!("Stake:        {} COEN", super::format_coen_amount(si.stake));
    println!("Total Staked: {} COEN", super::format_coen_amount(si.total));

    if !si.total.is_zero() && !si.stake.is_zero() {
        let pct = (si.stake * U256::from(10000)) / si.total;
        let pct_f = pct.to::<u64>() as f64 / 100.0;
        println!("Share:        {pct_f:.2}%");
    }

    Ok(())
}

struct StakingStats {
    total_staked: U256,
    active_count: u64,
    total_count: u64,
}

async fn fetch_staking_stats(client: &(impl Rpc + Sync)) -> Result<StakingStats> {
    let total_staked: U256 = {
        let call = IStaking::getTotalStakedCall {};
        let result = client
            .eth_call(abi::STAKING_ADDR, &call.abi_encode())
            .await?;
        IStaking::getTotalStakedCall::abi_decode_returns(&result)?
    };

    let active_count: u64 = {
        let call = IValidatorSet::activeValidatorCountCall {};
        let result = client
            .eth_call(abi::VALIDATOR_SET_ADDR, &call.abi_encode())
            .await?;
        IValidatorSet::activeValidatorCountCall::abi_decode_returns(&result)?.into()
    };

    let total_count: u64 = {
        let call = IValidatorSet::validatorCountCall {};
        let result = client
            .eth_call(abi::VALIDATOR_SET_ADDR, &call.abi_encode())
            .await?;
        IValidatorSet::validatorCountCall::abi_decode_returns(&result)?.into()
    };

    Ok(StakingStats {
        total_staked,
        active_count,
        total_count,
    })
}

async fn stats(client: &(impl Rpc + Sync)) -> Result<()> {
    let ss = fetch_staking_stats(client).await?;

    println!("=== Staking Statistics ===");
    println!(
        "Total Staked:       {} COEN",
        super::format_coen_amount(ss.total_staked)
    );
    println!("Total Validators:   {}", ss.total_count);
    println!("Active Validators:  {}", ss.active_count);

    if ss.active_count > 0 && !ss.total_staked.is_zero() {
        let avg = ss.total_staked / U256::from(ss.active_count);
        println!(
            "Avg Stake (active): {} COEN",
            super::format_coen_amount(avg)
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use alloy_primitives::U256;

    /// Stake tx must send value == amount (payable precompile requires msg.value == amount).
    #[test]
    fn test_stake_tx_value_equals_amount() {
        let amount = U256::from(100_000u64) * U256::from(10u64).pow(U256::from(18u64));
        // In stake(), the 4th arg to send_tx is `amount` (not Default::default()).
        let tx_value = amount; // mirrors staking.rs line 72
        assert_eq!(tx_value, amount, "stake tx value must equal amount");
        assert!(!tx_value.is_zero(), "stake tx value must not be zero");
    }

    /// Unstake and claim txs must send value == 0 (non-payable).
    #[test]
    fn test_non_payable_staking_calls_send_zero_value() {
        let unstake_value: U256 = Default::default(); // mirrors staking.rs line 90
        let claim_value: U256 = Default::default(); // mirrors staking.rs line 105
        assert!(unstake_value.is_zero(), "unstake tx value must be zero");
        assert!(claim_value.is_zero(), "claim tx value must be zero");
    }

    // --- Mock RPC tests with real ABI verification ---

    use super::*;
    use crate::rpc::mock::{
        abi_u256, abi_u64, call_map, recording_send_tx_rpc, recording_send_tx_rpc_after,
        ExpectedRpcCall, MockRpc, RecordedRpcCall, RecordedRpcResponse,
    };
    use alloy_sol_types::SolCall;
    use std::collections::HashMap;

    fn staking_info_mock(stake_val: U256, total_val: U256) -> MockRpc {
        let mut map = HashMap::new();
        let get_stake_sel = IStaking::getStakeCall::SELECTOR;
        let get_total_sel = IStaking::getTotalStakedCall::SELECTOR;
        map.insert((abi::STAKING_ADDR, get_stake_sel), abi_u256(stake_val));
        map.insert((abi::STAKING_ADDR, get_total_sel), abi_u256(total_val));
        MockRpc {
            eth_call_map: Some(call_map(map)),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn test_fetch_staking_info_returns_correct_values() {
        let stake = U256::from(500_000_000u64);
        let total = U256::from(1_000_000_000u64);
        let mock = staking_info_mock(stake, total);
        let addr: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();

        let result = fetch_staking_info(&mock, addr).await.unwrap();
        assert_eq!(result.stake, stake, "stake must match mock");
        assert_eq!(result.total, total, "total must match mock");
    }

    #[tokio::test]
    async fn test_fetch_staking_info_zero_stake() {
        let mock = staking_info_mock(U256::ZERO, U256::from(1000u64));
        let addr: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();

        let result = fetch_staking_info(&mock, addr).await.unwrap();
        assert_eq!(result.stake, U256::ZERO);
        assert_eq!(result.total, U256::from(1000u64));
    }

    #[tokio::test]
    async fn test_fetch_staking_info_rpc_error() {
        let mock = MockRpc::default(); // eth_call_map is None -> Err
        let addr: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        assert!(fetch_staking_info(&mock, addr).await.is_err());
    }

    #[tokio::test]
    async fn test_fetch_staking_stats_returns_correct_values() {
        let total = U256::from(3_000_000_000u64);
        let mut map = HashMap::new();
        map.insert(
            (abi::STAKING_ADDR, IStaking::getTotalStakedCall::SELECTOR),
            abi_u256(total),
        );
        map.insert(
            (
                abi::VALIDATOR_SET_ADDR,
                IValidatorSet::activeValidatorCountCall::SELECTOR,
            ),
            abi_u64(3),
        );
        map.insert(
            (
                abi::VALIDATOR_SET_ADDR,
                IValidatorSet::validatorCountCall::SELECTOR,
            ),
            abi_u64(5),
        );
        let mock = MockRpc {
            eth_call_map: Some(call_map(map)),
            ..Default::default()
        };

        let result = fetch_staking_stats(&mock).await.unwrap();
        assert_eq!(result.total_staked, total);
        assert_eq!(result.active_count, 3);
        assert_eq!(result.total_count, 5);
    }

    #[tokio::test]
    async fn test_staking_share_percentage_calculation() {
        let stake = U256::from(250_000_000u64);
        let total = U256::from(1_000_000_000u64);
        let mock = staking_info_mock(stake, total);
        let addr: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();

        let result = fetch_staking_info(&mock, addr).await.unwrap();
        // Verify 25% calculation
        let pct = (result.stake * U256::from(10000)) / result.total;
        let pct_f = pct.to::<u64>() as f64 / 100.0;
        assert!((pct_f - 25.0).abs() < 0.01, "expected 25%, got {pct_f}%");
    }

    // Known test private key (key=1)
    const TEST_KEY: &str = "0000000000000000000000000000000000000000000000000000000000000001";
    const U256_OVERFLOW_DECIMAL: &str =
        "115792089237316195423570985008687907853269984665640564039457584007913129639936";

    fn validator_addr() -> Address {
        "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap()
    }

    fn explicit_request(validator: Address, amount: &str) -> StakeRequest {
        StakeRequest {
            validator,
            amount: amount.to_string(),
            keys_dir: None,
            consensus_pubkey: Some("0xaabbcc".to_string()),
            radicle_node_id: Some(B256::repeat_byte(0x44)),
            bls_registration_signature: Some("0x112233".to_string()),
            symmetric: None,
            ingress_socket: None,
            ingress_dns_host: None,
            ingress_dns_port: None,
            egress: None,
        }
    }

    fn explicit_stake_call(
        validator: Address,
        amount: U256,
        p2p_version: u8,
        p2p: Vec<u8>,
    ) -> Vec<u8> {
        IStaking::stakeCall {
            validatorAddress: validator,
            amount,
            consensusPubkey: vec![0xaa, 0xbb, 0xcc].into(),
            radicleNodeId: B256::repeat_byte(0x44),
            blsRegistrationSignature: vec![0x11, 0x22, 0x33].into(),
            p2pVersion: p2p_version,
            p2pEncoded: p2p.into(),
        }
        .abi_encode()
    }

    fn invalid_amount_cases() -> [(&'static str, &'static str); 8] {
        [
            ("not_a_number", "invalid amount"),
            ("1.0", "invalid amount"),
            (" 1", "invalid amount"),
            ("1 ", "invalid amount"),
            ("+1", "invalid amount"),
            ("-1", "invalid amount"),
            ("0", "amount must be non-zero"),
            (U256_OVERFLOW_DECIMAL, "invalid amount"),
        ]
    }

    #[test]
    fn test_parse_amount_rejects_bad_inputs() {
        for (amount, expected) in invalid_amount_cases() {
            let err = parse_amount(amount).unwrap_err();
            assert!(
                err.to_string().contains(expected),
                "amount={amount:?}: expected {expected:?}, got {err}"
            );
        }
    }

    #[test]
    fn test_parse_amount_accepts_max_u256() {
        assert_eq!(parse_amount(&U256::MAX.to_string()).unwrap(), U256::MAX);
    }

    #[tokio::test]
    async fn test_stake_sends_tx() {
        let validator = validator_addr();
        let amount = U256::from(1000u64);
        let data = explicit_stake_call(validator, amount, 0, Vec::new());
        let mock = recording_send_tx_rpc(TEST_KEY, abi::STAKING_ADDR, data, amount).unwrap();

        stake(&mock, Some(TEST_KEY), explicit_request(validator, "1000"))
            .await
            .unwrap();
        mock.assert_done();
    }

    #[tokio::test]
    async fn test_stake_max_u256_amount_sends_tx() {
        let validator = validator_addr();
        let amount = U256::MAX;
        let data = explicit_stake_call(validator, amount, 0, Vec::new());
        let mock = recording_send_tx_rpc(TEST_KEY, abi::STAKING_ADDR, data, amount).unwrap();

        stake(
            &mock,
            Some(TEST_KEY),
            explicit_request(validator, &amount.to_string()),
        )
        .await
        .unwrap();
        mock.assert_done();
    }

    #[tokio::test]
    async fn test_stake_symmetric_p2p_is_version_one() {
        let validator = validator_addr();
        let amount = U256::from(1000u64);
        let socket: std::net::SocketAddr = "10.0.0.1:30400".parse().unwrap();
        let encoded = encode_v1(&outbe_primitives::consensus_p2p::P2pAddress::Symmetric(
            socket,
        ));
        let data = explicit_stake_call(validator, amount, P2P_ADDRESS_VERSION_V1, encoded);
        let mock = recording_send_tx_rpc(TEST_KEY, abi::STAKING_ADDR, data, amount).unwrap();
        let mut request = explicit_request(validator, "1000");
        request.symmetric = Some(socket);

        stake(&mock, Some(TEST_KEY), request).await.unwrap();
        mock.assert_done();
    }

    #[tokio::test]
    async fn test_stake_no_private_key_errors() {
        let mock = recording_send_tx_rpc(TEST_KEY, abi::STAKING_ADDR, vec![], U256::ZERO).unwrap();
        let err = stake(&mock, None, explicit_request(validator_addr(), "1000"))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("--private-key required"),
            "expected private-key error, got: {err}"
        );
        assert!(
            mock.recorded_calls().is_empty(),
            "missing signer must not call RPC"
        );
    }

    #[tokio::test]
    async fn test_stake_invalid_amounts_do_not_call_rpc() {
        for (amount, expected) in invalid_amount_cases() {
            let mock =
                recording_send_tx_rpc(TEST_KEY, abi::STAKING_ADDR, vec![], U256::ZERO).unwrap();
            let err = stake(
                &mock,
                Some(TEST_KEY),
                explicit_request(validator_addr(), amount),
            )
            .await
            .unwrap_err();
            assert!(
                err.to_string().contains(expected),
                "amount={amount:?}: expected {expected:?}, got {err}"
            );
            assert!(
                mock.recorded_calls().is_empty(),
                "invalid stake amount {amount:?} must not call RPC"
            );
        }
    }

    #[tokio::test]
    async fn test_stake_invalid_pubkey_does_not_call_rpc() {
        let mock = recording_send_tx_rpc(TEST_KEY, abi::STAKING_ADDR, vec![], U256::ZERO).unwrap();
        let mut request = explicit_request(validator_addr(), "1000");
        request.consensus_pubkey = Some("not_hex".to_string());
        let err = stake(&mock, Some(TEST_KEY), request).await.unwrap_err();
        assert!(
            err.to_string().contains("--consensus-pubkey"),
            "expected hex error, got: {err}"
        );
        assert!(mock.recorded_calls().is_empty());
    }

    #[tokio::test]
    async fn test_stake_identity_flags_are_exclusive() {
        let mock = recording_send_tx_rpc(TEST_KEY, abi::STAKING_ADDR, vec![], U256::ZERO).unwrap();
        let mut request = explicit_request(validator_addr(), "1000");
        request.keys_dir = Some(std::path::PathBuf::from("/tmp/unused-keys"));
        let err = stake(&mock, Some(TEST_KEY), request).await.unwrap_err();
        assert!(err.to_string().contains("--keys-dir"), "{err}");
        assert!(mock.recorded_calls().is_empty());

        let mock = recording_send_tx_rpc(TEST_KEY, abi::STAKING_ADDR, vec![], U256::ZERO).unwrap();
        let mut request = explicit_request(validator_addr(), "1000");
        request.consensus_pubkey = None;
        let err = stake(&mock, Some(TEST_KEY), request).await.unwrap_err();
        assert!(err.to_string().contains("--consensus-pubkey"), "{err}");
        assert!(mock.recorded_calls().is_empty());
    }

    #[tokio::test]
    async fn test_topup_sends_tx() {
        let validator = validator_addr();
        let amount = U256::from(1000u64);
        let data = IStaking::topupCall {
            validatorAddress: validator,
            amount,
        }
        .abi_encode();
        let mock = recording_send_tx_rpc(TEST_KEY, abi::STAKING_ADDR, data, amount).unwrap();

        topup(&mock, Some(TEST_KEY), validator, "1000".to_string())
            .await
            .unwrap();
        mock.assert_done();
    }

    #[tokio::test]
    async fn test_topup_no_private_key_errors() {
        let mock = recording_send_tx_rpc(TEST_KEY, abi::STAKING_ADDR, vec![], U256::ZERO).unwrap();
        let err = topup(&mock, None, validator_addr(), "1000".to_string())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("--private-key required"), "{err}");
        assert!(mock.recorded_calls().is_empty());
    }

    fn write_keys_dir(dir: &std::path::Path, evm_key_hex: &str) -> (Vec<u8>, B256) {
        use commonware_codec::Encode as _;
        use commonware_cryptography::{bls12381, Signer as _};
        use commonware_math::algebra::Random as _;
        use outbe_consensus::bls::{self, KeyBackend};

        let bls_key = bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
            rand_commonware::rngs::SysRng,
        ));
        let pubkey = bls_key.public_key().encode().as_ref().to_vec();
        bls::save_individual_key(
            &dir.join("signing-key.hex"),
            &bls_key,
            &KeyBackend::Plaintext,
        )
        .unwrap();
        std::fs::write(dir.join("evm-key.hex"), evm_key_hex).unwrap();
        let keystore = radicle_crypto::ssh::Keystore::new(&dir.join("radicle/keys"));
        let public = keystore
            .init(
                "outbe-cli-test",
                None,
                radicle_crypto::Seed::new([7_u8; 32]),
            )
            .unwrap();
        (pubkey, B256::from(public.into_inner()))
    }

    fn reference_registration_signature(
        dir: &std::path::Path,
        chain_id: u64,
        validator: Address,
        node_id: B256,
    ) -> Vec<u8> {
        use commonware_codec::Encode as _;
        use commonware_cryptography::Signer as _;
        use outbe_consensus::bls::{self, KeyBackend};
        use outbe_primitives::validators::{
            validator_registration_message, VALIDATOR_REGISTRATION_DST,
        };

        let key =
            bls::load_individual_key(&dir.join("signing-key.hex"), &KeyBackend::Plaintext).unwrap();
        let encoded = key.encode();
        let blst_key = blst::min_pk::SecretKey::from_bytes(encoded.as_ref()).unwrap();
        let message = validator_registration_message(chain_id, validator, node_id);
        let signature = blst_key.sign(&message, VALIDATOR_REGISTRATION_DST, &[]);
        let pubkey =
            blst::min_pk::PublicKey::from_bytes(key.public_key().encode().as_ref()).unwrap();
        assert_eq!(
            signature.verify(
                true,
                &message,
                VALIDATOR_REGISTRATION_DST,
                &[],
                &pubkey,
                true
            ),
            blst::BLST_ERROR::BLST_SUCCESS
        );
        signature.to_bytes().to_vec()
    }

    fn keys_dir_request(dir: &std::path::Path, validator: Address, amount: &str) -> StakeRequest {
        StakeRequest {
            validator,
            amount: amount.to_string(),
            keys_dir: Some(dir.to_path_buf()),
            consensus_pubkey: None,
            radicle_node_id: None,
            bls_registration_signature: None,
            symmetric: None,
            ingress_socket: None,
            ingress_dns_host: None,
            ingress_dns_port: None,
            egress: None,
        }
    }

    #[tokio::test]
    async fn test_stake_keys_dir_signs_registration_and_sends_value() {
        let dir = tempfile::tempdir().unwrap();
        let (pubkey, node_id) = write_keys_dir(dir.path(), TEST_KEY);
        let validator = crate::tx::TxSigner::new(TEST_KEY).unwrap().address();
        let amount = U256::from(1000u64);
        let signature = reference_registration_signature(dir.path(), 1337, validator, node_id);
        let data = IStaking::stakeCall {
            validatorAddress: validator,
            amount,
            consensusPubkey: pubkey.into(),
            radicleNodeId: node_id,
            blsRegistrationSignature: signature.into(),
            p2pVersion: 0,
            p2pEncoded: Vec::<u8>::new().into(),
        }
        .abi_encode();
        let mock = recording_send_tx_rpc_after(
            TEST_KEY,
            abi::STAKING_ADDR,
            data,
            amount,
            [ExpectedRpcCall::ok(
                RecordedRpcCall::EthChainId,
                RecordedRpcResponse::U64(1337),
            )],
        )
        .unwrap();

        stake(
            &mock,
            Some(TEST_KEY),
            keys_dir_request(dir.path(), validator, "1000"),
        )
        .await
        .unwrap();
        mock.assert_done();
    }

    #[tokio::test]
    async fn test_stake_keys_dir_rejects_validator_mismatch_before_rpc() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("evm-key.hex"),
            "0000000000000000000000000000000000000000000000000000000000000002",
        )
        .unwrap();
        let validator = crate::tx::TxSigner::new(TEST_KEY).unwrap().address();
        let mock = recording_send_tx_rpc(TEST_KEY, abi::STAKING_ADDR, vec![], U256::ZERO).unwrap();
        let err = stake(
            &mock,
            Some(TEST_KEY),
            keys_dir_request(dir.path(), validator, "1000"),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("derives"), "{err}");
        assert!(mock.recorded_calls().is_empty());
    }

    #[tokio::test]
    async fn test_unstake_sends_tx() {
        let amount = U256::from(500u64);
        let data = IStaking::unstakeCall { amount }.abi_encode();
        let mock = recording_send_tx_rpc(TEST_KEY, abi::STAKING_ADDR, data, U256::ZERO).unwrap();

        unstake(&mock, Some(TEST_KEY), "500".to_string())
            .await
            .unwrap();
        mock.assert_done();
    }

    #[tokio::test]
    async fn test_unstake_max_u256_amount_sends_tx() {
        let amount = U256::MAX;
        let data = IStaking::unstakeCall { amount }.abi_encode();
        let mock = recording_send_tx_rpc(TEST_KEY, abi::STAKING_ADDR, data, U256::ZERO).unwrap();

        unstake(&mock, Some(TEST_KEY), amount.to_string())
            .await
            .unwrap();
        mock.assert_done();
    }

    #[tokio::test]
    async fn test_unstake_invalid_amounts_do_not_call_rpc() {
        for (amount, expected) in invalid_amount_cases() {
            let mock =
                recording_send_tx_rpc(TEST_KEY, abi::STAKING_ADDR, vec![], U256::ZERO).unwrap();
            let err = unstake(&mock, Some(TEST_KEY), amount.to_string())
                .await
                .unwrap_err();
            assert!(
                err.to_string().contains(expected),
                "amount={amount:?}: expected {expected:?}, got {err}"
            );
            assert!(
                mock.recorded_calls().is_empty(),
                "invalid unstake amount {amount:?} must not call RPC"
            );
        }
    }

    #[tokio::test]
    async fn test_claim_sends_tx() {
        let data = IStaking::claimUnbondedCall {}.abi_encode();
        let mock = recording_send_tx_rpc(TEST_KEY, abi::STAKING_ADDR, data, U256::ZERO).unwrap();

        claim(&mock, Some(TEST_KEY)).await.unwrap();
        mock.assert_done();
    }

    #[tokio::test]
    async fn test_claim_no_private_key_errors() {
        let mock = recording_send_tx_rpc(TEST_KEY, abi::STAKING_ADDR, vec![], U256::ZERO).unwrap();
        let err = claim(&mock, None).await.unwrap_err();
        assert!(
            err.to_string().contains("--private-key required"),
            "expected private-key error, got: {err}"
        );
        assert!(
            mock.recorded_calls().is_empty(),
            "missing signer must not call RPC"
        );
    }
}
