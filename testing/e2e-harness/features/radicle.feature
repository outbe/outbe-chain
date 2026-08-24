@radicle @ocomp @tee @sgx-no-attest @sudo @min-validators-4
Feature: Validator-managed Radicle storage
  Validators discover one another from finalized chain identity and replicate a
  permissionlessly registered public repository without joining public Radicle.

  Scenario: A public repository survives topology and process changes
    Given a fresh four-validator Radicle localnet
    Then Radicle genesis and the signed validator mesh are ready
    When an offline user repository is registered by a funded non-validator
    Then every validator reports the finalized repository as pending while consensus advances
    When the independent source comes online and publishes its Git update
    Then every validator reports the repository available with its issue patch and Git head
    When validator 0 changes only its Radicle peer port
    Then the signed endpoint converges without restarting validator 0
    When validator 1 Radicle sidecar is stopped
    Then consensus advances while validator 1 reports Radicle degraded
    When validator 1 Radicle sidecar is restarted
    Then validator 1 returns to Radicle ready with the same identity
    When validator 2 node restarts while its Radicle sidecar stays alive
    Then validator 2 returns to Radicle ready without restarting its sidecar
    When a full node is promoted into the validator set
    Then it publishes no endpoint before activation and joins the signed mesh after activation
    And no validator has a public Radicle seed session
