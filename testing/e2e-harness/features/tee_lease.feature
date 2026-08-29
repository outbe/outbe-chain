@tee @sgx-no-attest @sudo @min-validators-4
Feature: Manual consensus-enforced TEE lease lifecycle
  The release SGX/no-attest lane keeps the production fourteen-day lease and
  crosses its seven-day window with the testnet-only consensus clock.

  Scenario: Three validators renew while one validator and one FullNode expire and recover
    Given a fresh four-validator manual TEE lease localnet
    And one role-neutral FullNode joins with the committee lease deadline
    When finalized consensus time enters the final seven-day renewal window
    And validators 0, 1, and 2 manually renew their enclave leases
    Then their exact next deadlines finalize without changing any permanent offer key
    When finalized consensus time reaches the original lease deadline
    Then validator 3 is jailed without slash while three validators keep finalizing
    And the expired FullNode fail-stops and late renewal is rejected for both missed nodes
    When the jailed validator is excluded at the normal DKG boundary
    And validator 3 unjails and both expired nodes complete fresh TEE join
    Then validator 3 returns only after readiness and DKG while the FullNode resumes sync
