"""Network identities, defaults and configuration validation."""

from __future__ import annotations
import re
from pathlib import Path
from typing import Any
import launch_bundle  # noqa: E402  (sibling module, path set just above)


SCRIPT_DIR = Path(__file__).resolve().parent


REPO_ROOT = SCRIPT_DIR.parent


BASE_PROFILE_PATH = SCRIPT_DIR / "testnet.yaml"


DEFAULT_CHAIN_ID = 424242


DEVNET_CHAIN_ID = 424242


TESTNET_CHAIN_ID = 54322345


MAINNET_CHAIN_ID = 676


NETWORK_IDENTITIES = {
    "devnet": (DEVNET_CHAIN_ID, "outbe-devnet-1"),
    "testnet": (TESTNET_CHAIN_ID, "outbe-testnet-1"),
    "mainnet": (MAINNET_CHAIN_ID, "outbe-mainnet-1"),
}


DEFAULT_GAS_LIMIT = "0x1c9c380"


DEFAULT_EPOCH_LENGTH_BLOCKS = 300


DEFAULT_DKG_PREPARE_WINDOW_BLOCKS = 30


DEFAULT_DKG_ACTIVATION_GRACE_BLOCKS = 30


DEFAULT_CONSENSUS_P2P_PORT = 30400


DEFAULT_PREFUND_COEN_UNITS = 10_000 * 10**18


FOUNDER_COUNT = 4


SECP256K1_P = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F


SECP256K1_N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141


SECP256K1_G = (
    0x79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798,
    0x483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8,
)


SEED_SECTIONS = (
    # Protocol timings. seed_genesis copies these into config.outbeProtocol.
    # The runtime reads the Metadosis day windows
    # from there. If they are absent, the chain runs on its built-in defaults.
    # A devnet or a test network rarely wants those defaults.
    "protocol_constants",
    "balance",
    "staking",
    "rewards",
    "validator_set",
    "radicle_registry",
    "metadosis",
    "oracle",
    "tributes",
    "intex_profile",
    "gem_profile",
    "nod_profile",
    "credis_profile",
    "contracts",
    "tee_policy",
)


TOP_LEVEL_KEYS = {
    "network",
    "validators",
    "keys_dir",
    "chain_id",
    "tee",
    "output",
    "chain_binary",
    "keygen_binary",
    "consensus_p2p_port",
    "gas_limit",
    "timestamp",
    "epoch_length_blocks",
    "dkg_prepare_window_blocks",
    "dkg_activation_grace_blocks",
    "prefund_coen_units",
    "worldwide_day",
    "fresh_metadosis",
    "contracts_dir",
    "canon_dir",
    "enclave_image",
    "enclave_dir",
    "enclave_runner",
    "enclave_sgx",
    "signed_enclave_dir",
    "allow_stale_timestamp",
    "node_binary",
    "radicle_binary",
    "radicle_external_inbound_reserve",
    "feeder_binary",
    "remote_base_dir",
    "remote_keys_dir",
    "price_provider",
    "public_rpc_port",
    "public_radicle_status_port",
    "price_feed_rest",
    "price_feed_websocket",
    "feeder_pairs",
    *launch_bundle.DEFAULT_PORTS,
    *SEED_SECTIONS,
}


TEE_KEYS = {
    "mode",
    "mrenclave",
    "mrsigner",
    "isv_prod_id",
    "minimum_isv_svn",
    "minimum_tcb_evaluation_data_number",
}


def deep_merge(base: Any, override: Any) -> Any:
    """Scalars and lists replace; mappings merge key by key."""
    if isinstance(base, dict) and isinstance(override, dict):
        merged = dict(base)
        for key, value in override.items():
            merged[key] = deep_merge(base.get(key), value) if key in base else value
        return merged
    return override


def validate_config(config: dict[str, Any]) -> None:
    if "intex_factory" in config:
        raise ValueError("intex_factory was renamed to intex_profile")
    unknown = sorted(set(config) - TOP_LEVEL_KEYS)
    if unknown:
        raise ValueError(
            f"unknown key(s): {', '.join(unknown)}; "
            f"allowed: {', '.join(sorted(TOP_LEVEL_KEYS))}"
        )
    _validate_founders(config)
    _validate_tee(config)
    _validate_ports(config)


def _validate_founders(config):
    hosts = config.get("validators")
    if not isinstance(hosts, list) or len(hosts) != FOUNDER_COUNT:
        raise ValueError(
            f"`validators` must list exactly {FOUNDER_COUNT} hosts "
            f"(the bootstrap network profile uses four founders)"
        )
    for index, host in enumerate(hosts):
        if not isinstance(host, str) or not host.strip():
            raise ValueError(f"validators[{index}] must be a host or IP string")
    if "keys_dir" not in config:
        raise ValueError("`keys_dir` is required: it holds validator-N/ key material")


def _validate_tee(config):
    tee = config.get("tee")
    if not isinstance(tee, dict):
        raise ValueError("`tee` section with `mode` is required")
    unknown_tee = sorted(set(tee) - TEE_KEYS)
    if unknown_tee:
        raise ValueError(f"unknown tee key(s): {', '.join(unknown_tee)}")
    network, chain_id, _ = network_identity(config)
    mode = tee.get("mode")
    if mode not in ("gramine-direct-dev", "dcap-required"):
        raise ValueError("tee.mode must be gramine-direct-dev or dcap-required")
    if network == "mainnet":
        _validate_mainnet(config, mode)
    if mode == "gramine-direct-dev" and chain_id not in (
        DEVNET_CHAIN_ID,
        TESTNET_CHAIN_ID,
    ):
        raise ValueError(
            f"tee.mode gramine-direct-dev requires the devnet or testnet chain id "
            f"({DEVNET_CHAIN_ID} or {TESTNET_CHAIN_ID}), not {chain_id}"
        )
    if mode == "dcap-required":
        if chain_id not in (DEVNET_CHAIN_ID, TESTNET_CHAIN_ID, MAINNET_CHAIN_ID):
            raise ValueError(
                f"tee.mode dcap-required requires a canonical Outbe chain id, not {chain_id}"
            )
        image = str(config.get("enclave_image", ""))
        if re.fullmatch(r"[^\s]+@sha256:[0-9a-f]{64}", image) is None:
            raise ValueError(
                "tee.mode dcap-required needs `enclave_image` pinned to an immutable "
                "digest (name@sha256:<64 lowercase hex>), not a mutable tag"
            )


def _validate_ports(config):
    # Every endpoint the launch scripts bind, on one machine.
    ports = {
        name: port_value
        for name in launch_bundle.DEFAULT_PORTS
        if (port_value := int(config.get(name, launch_bundle.DEFAULT_PORTS[name])))
    }
    consensus_port = int(config.get("consensus_p2p_port", DEFAULT_CONSENSUS_P2P_PORT))
    ports["consensus_p2p_port"] = consensus_port
    seen: dict[int, str] = {}
    for name, port_value in sorted(ports.items()):
        if not 1 <= port_value <= 65535:
            raise ValueError(f"{name} is outside 1..65535: {port_value}")
        if port_value in seen:
            raise ValueError(
                f"port collision: {seen[port_value]} and {name} both use {port_value}"
            )
        seen[port_value] = name


def _validate_mainnet(config, mode):
    if mode != "dcap-required":
        raise ValueError("Mainnet requires tee.mode dcap-required")
    if "protocol_constants" in config:
        raise ValueError(
            "Mainnet forbids protocol_constants overrides and uses canonical production defaults"
        )
    endpoint = str(config.get("price_feed_rest", ""))
    if not endpoint:
        raise ValueError(
            "Mainnet requires an explicit production price feed REST endpoint"
        )
    if "testnet" in endpoint.lower():
        raise ValueError("Mainnet may not use a testnet price endpoint")


def network_identity(config: dict[str, Any]) -> tuple[str, int, str]:
    chain_id = int(config.get("chain_id", DEFAULT_CHAIN_ID))
    inferred = next(
        (
            name
            for name, (known_id, _) in NETWORK_IDENTITIES.items()
            if known_id == chain_id
        ),
        None,
    )
    if inferred is None:
        raise ValueError(f"unknown Outbe chain id {chain_id}")
    requested = config.get("network")
    if requested is None:
        if inferred == "mainnet":
            raise ValueError(
                "Mainnet chain id 676 requires explicit `network: mainnet`"
            )
        requested = inferred
    if requested not in NETWORK_IDENTITIES:
        raise ValueError(f"unknown Outbe network {requested!r}")
    expected_id, chain_name = NETWORK_IDENTITIES[requested]
    if chain_id != expected_id:
        raise ValueError(f"{requested} requires chain id {expected_id}, not {chain_id}")
    return requested, chain_id, chain_name


MAX_GENESIS_AGE_SECONDS = 6 * 60 * 60


def resolve_binary(config: dict[str, Any], key: str, name: str) -> str:
    explicit = config.get(key)
    candidates = (
        [Path(str(explicit)), REPO_ROOT / str(explicit)]
        if explicit
        else [
            REPO_ROOT / "target" / "release" / name,
            REPO_ROOT / "target" / "debug" / name,
        ]
    )
    for candidate in candidates:
        if candidate.is_file():
            return str(candidate)
    raise ValueError(f"{name} binary not found; build it or set `{key}` in the yaml")
