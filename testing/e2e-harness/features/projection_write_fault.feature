@tee @sgx-no-attest @sudo @min-validators-4 @ocomp @projection-rocksdb-fault
Feature: RocksDB projection write outage
  A validator must retry its durable projection batch while the committee
  continues to finalize, then catch up when disk capacity returns.

  Scenario: A full projection volume recovers without losing a finalized Tribute
    Given a four-validator bounded Tribute localnet with a scenario-owned RocksDB volume
    When validator 0 RocksDB runs out of space during an encrypted Tribute
    Then the finalized Tribute reaches the same durable projection on every validator
