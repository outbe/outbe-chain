@radicle @ocomp @tee @sgx-no-attest @sudo @min-validators-4
Feature: Validator-managed Radicle storage
  Validators discover one another from finalized chain identity and replicate a
  permissionlessly registered public repository without joining public Radicle.

  Scenario: The network bootstraps its BLS-owned governments repository
    Given a fresh four-validator governments localnet
    Then every validator has the same finalized governments bootstrap and native owner refs

  @governments-patch-vote
  Scenario: The network votes to merge an exact governments patch and new peers verify its history
    Given a fresh four-validator governments patch-vote localnet
    Then every validator has the same finalized governments bootstrap and native owner refs
    And the governments validators form the signed mesh
    When an independent author publishes a governments patch
    Then every validator sees the exact author revision while governments main remains unchanged
    When the current validators vote to accept that governments revision
    Then every validator verifies the finalized BLS Review and Merge for that revision
    When a fresh validator joins after the governments merge
    Then the fresh validator verifies the accepted governments history without voting again
    And a fresh ordinary Radicle reader fetches the merged governments history

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
    When the independent source publishes a second Git head during the sidecar outage
    Then the three live validators serve the second head while validator 1 does not
    When validator 1 Radicle sidecar is restarted
    Then validator 1 returns to Radicle ready with the same identity
    When validator 2 node restarts while its Radicle sidecar stays alive
    Then validator 2 returns to Radicle ready without restarting its sidecar
    When a full node is promoted into the validator set
    Then it publishes no endpoint before activation and joins the signed mesh after activation
    And no validator has a public Radicle seed session
