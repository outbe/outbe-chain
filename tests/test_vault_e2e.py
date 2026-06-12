import pytest
from ape import networks

from helpers.helper import deployer, tx_kwargs
from helpers.vault_provider import deploy_local_stack, vault_contract


DEPOSIT_AMOUNT = 1_000 * 10**18
WITHDRAW_AMOUNT = 400 * 10**18


@pytest.fixture
def bootstrap():
    with networks.parse_network_choice("ethereum:localhost:node"):
        owner = deployer()
        stack = deploy_local_stack(owner)
        receiver = owner.deploy(vault_contract("TokenBundleReceiverMock"), **tx_kwargs())
        yield owner, stack.asset, stack.vault, stack.provider, receiver


def test_vault_provider_e2e_flow(bootstrap):
    owner, asset, vault, provider, receiver = bootstrap

    asset.mint(owner.address, DEPOSIT_AMOUNT, sender=owner, **tx_kwargs())
    asset.approve(provider.address, DEPOSIT_AMOUNT, sender=owner, **tx_kwargs())

    provider.depositLiquidity(asset.address, DEPOSIT_AMOUNT, sender=owner, **tx_kwargs())

    provider_shares = provider.sharesBalance(vault.address)
    assert provider_shares > 0
    assert vault.balanceOf(provider.address) == provider_shares
    assert asset.balanceOf(provider.address) == 0

    required_shares = vault.previewWithdraw(WITHDRAW_AMOUNT)

    provider.withdrawLiquidity(
        asset.address,
        WITHDRAW_AMOUNT,
        receiver.address,
        sender=owner,
        **tx_kwargs(),
    )

    assert asset.balanceOf(receiver.address) == WITHDRAW_AMOUNT
    assert provider.sharesBalance(vault.address) == provider_shares - required_shares
    assert vault.balanceOf(provider.address) == provider_shares - required_shares
