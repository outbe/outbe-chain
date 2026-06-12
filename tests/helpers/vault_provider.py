from dataclasses import dataclass

from ape import project

from helpers.helper import tx_kwargs


VAULT_CONTRACT_PATHS = {
    "VaultProvider": "contracts/vault/src/VaultProvider.sol",
    "ERC20Mock": "contracts/vault/test/mocks/ERC20Mock.sol",
    "ERC4626Mock": "contracts/vault/test/mocks/ERC4626Mock.sol",
    "TokenBundleReceiverMock": "contracts/vault/test/mocks/TokenBundleReceiverMock.sol",
}
LIQUIDITY_SOURCE_NOD_STRIKE_PRICE = 1
LIQUIDITY_TARGET_CREDIS = 1


@dataclass(frozen=True)
class VaultProviderStack:
    asset: object
    vault: object
    provider: object


def vault_contract(name):
    return project.load_contracts(VAULT_CONTRACT_PATHS[name])[name]


def deploy_vault_provider(owner):
    provider = owner.deploy(vault_contract("VaultProvider"), **tx_kwargs())
    provider.initialize(owner.address, sender=owner, **tx_kwargs())
    return provider


def setup_vault_provider(provider, vault, owner):
    provider.addVault(vault.address, sender=owner, **tx_kwargs())
    provider.addLiquiditySource(
        owner.address,
        LIQUIDITY_SOURCE_NOD_STRIKE_PRICE,
        sender=owner,
        **tx_kwargs(),
    )
    provider.addLiquidityTarget(
        owner.address,
        LIQUIDITY_TARGET_CREDIS,
        sender=owner,
        **tx_kwargs(),
    )


def deploy_local_stack(owner):
    asset = owner.deploy(vault_contract("ERC20Mock"), 18, **tx_kwargs())
    vault = owner.deploy(vault_contract("ERC4626Mock"), asset.address, **tx_kwargs())
    provider = deploy_vault_provider(owner)
    setup_vault_provider(provider, vault, owner)
    return VaultProviderStack(asset=asset, vault=vault, provider=provider)
