@tee @min-validators-4 @hyperlane-controller
Feature: Hyperlane controller signer and liveness recovery
  Scenario: A silent bridge signer is jailed and the local ISM follows the live committee
    Given a live committee with a controller-owned Hyperlane ISM fixture
    Then the controller mirrors all active validator signers into the ISM
    When a validator rotates its bridge signing key and submits verified checkpoints
    Then the ISM reflects the rotated signer and rejects invalid checkpoint signatures
    When three validators advance their checkpoint while one bridge agent stops
    Then three liveness windows jail only the silent validator and resync the ISM
