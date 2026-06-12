import os
from pathlib import Path

import pytest
from ape import accounts
from eth_account import Account

LOCALHOST_MAX_FEE = 10_000_000_000
LOCALHOST_MAX_PRIORITY_FEE = 0
LOCALHOST_GAS_LIMIT = 20_000_000


def env_values():
    values = {}
    env_file = Path(__file__).resolve().parents[2] / ".env"
    if env_file.exists():
        for line in env_file.read_text(encoding="utf8").splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, value = line.split("=", 1)
            values[key.strip()] = value.strip().strip("\"'")

    return {**values, **os.environ}


def test_address(env=None):
    env = env or env_values()
    address = env.get("TEST_ADDRESS")
    if not address:
        pytest.skip("set TEST_ADDRESS for localhost tests")

    return address


def private_key_deployer():
    env = env_values()
    private_key = env.get("TEST_PRIVATE_KEY")
    if not private_key:
        return None

    if not private_key.startswith("0x"):
        private_key = f"0x{private_key}"

    address = Account.from_key(private_key).address
    expected_address = test_address(env)
    if address.lower() != expected_address.lower():
        pytest.skip(f"TEST_PRIVATE_KEY derives {address}, expected {expected_address}")

    return accounts.init_test_account(1000, address, private_key)


def unlocked_test_address_deployer():
    address = test_address()
    try:
        return accounts[address]
    except KeyError:
        return None


def deployer():
    deployer = private_key_deployer()
    if deployer is None:
        deployer = unlocked_test_address_deployer()

    if deployer is not None:
        address = test_address()
        if deployer.address.lower() != address.lower():
            pytest.skip(f"localhost deployer must be {address}, got {deployer.address}")
        if deployer.balance == 0:
            pytest.skip(f"localhost deployer {deployer.address} has no native balance")
        return deployer

    address = test_address()
    pytest.skip(
        "localhost:8545 has no usable deployer; "
        f"set TEST_PRIVATE_KEY for funded test address {address}"
    )


def tx_kwargs():
    return {
        "gas": LOCALHOST_GAS_LIMIT,
        "max_fee": LOCALHOST_MAX_FEE,
        "max_priority_fee": LOCALHOST_MAX_PRIORITY_FEE,
    }
