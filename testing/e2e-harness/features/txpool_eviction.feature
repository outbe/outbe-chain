@min-validators-4
Feature: Transaction-pool eviction
  # A transaction that can never be mined must not live in the pool forever.
  # Two independent bounds own that guarantee, and this feature owns both:
  #   - parked (nonce-gapped) transactions age out on the pool lifetime, and
  #     RPC-submitted transactions get NO exemption from it;
  #   - pending transactions that survive two consecutive pool snapshots are
  #     evicted, which is what breaks the "included in every proposal, never
  #     finalized, re-injected forever" loop.
  # Eviction is node-local pool policy: it never affects block validity, and a
  # healthy transaction submitted alongside must keep being mined.

  @queued-lifetime
  Scenario: A transaction that can never be mined is evicted from every pool
    Given a fresh txpool-eviction localnet with a 6-block voting window
    And the committee has reached a usable height
    When an operator submits a transaction with an unreachable nonce
    Then the unreachable transaction sits in the pool
    And an ordinary transfer submitted alongside is mined
    When the pool lifetime elapses
    Then the unreachable transaction is gone from every validator's pool
    And the submitting validator logged the exact eviction identity and reason
    When the submitting validator restarts after the queued eviction
    Then the evicted transaction stays absent and the restarted committee finalizes
    And the committee is still producing blocks

  @tee @sgx-no-attest @sudo @pending-staleness
  Scenario: A non-proposing FullNode evicts an executable transaction after two canonical snapshots
    Given a fresh txpool-eviction localnet with a 6-block voting window
    And the committee has reached a usable height
    When an isolated production FullNode syncs from the committee for pending-pool testing
    And an operator submits one executable transaction only to the isolated FullNode
    Then the executable transaction remains pending through the first snapshot
    When the two-snapshot pending staleness window elapses
    Then the FullNode evicts the exact pending transaction as stale
    And the same nonce remains usable on the committee and finality continues
