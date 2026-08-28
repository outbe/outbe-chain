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

  Scenario: A transaction that can never be mined is evicted from every pool
    Given a fresh txpool-eviction localnet with a 6-block voting window
    And the committee has reached a usable height
    When an operator submits a transaction with an unreachable nonce
    Then the unreachable transaction sits in the pool
    And an ordinary transfer submitted alongside is mined
    When the pool lifetime elapses
    Then the unreachable transaction is gone from every validator's pool
    And the submitting validator logged the exact eviction identity and reason
    And the committee is still producing blocks
