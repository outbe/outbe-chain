@ocomp @tee @sgx-no-attest @sudo @min-validators-4
Feature: Off-chain computation public path
  @ocomp-public-prefix
  Scenario: A public encrypted Tribute becomes one finalized JobIntent
    Given a fresh four-validator OCOMP public measurement localnet
    When an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    Then Metadosis creates one finalized JobIntent from that public Tribute
    And all four OCOMP domains run their node-facing production roles

  @ocomp-public-apply
  # OCOMP-TEST-ID: OCM-PUB-001
  # OCOMP-TEST-ID: OCM-PUB-004
  Scenario: Four independent domains certify and atomically apply one public Lysis result
    Given a fresh four-validator OCOMP public measurement localnet
    When an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    When a fifth node syncs as a non-voting FullNode
    Then the fifth node has canonical state parity without OCOMP vote capability
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the production OCOMP domains process that finalized JobIntent
    Then three matching validator domains atomically apply Lysis and create the Nod
    And the keyless FullNode verifies the same finalized Nod body through its local proof path
    And all four OCOMP domains run their node-facing production roles
    When the completed full-result vote is retried and then mutated through public RPC
    Then the completed job and Nod generation are unchanged by both transactions

  @ocomp-e2e @ocomp-e2e-008
  # OCOMP-TEST-ID: OCM-E2E-008
  Scenario: A completed SGX generation survives node and compute-process restart and replay
    Given a fresh four-validator OCOMP public measurement localnet
    When an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the production OCOMP domains process that finalized JobIntent
    Then three matching validator domains atomically apply Lysis and create the Nod
    And all four OCOMP domains use the production basedir contract
    When validator 0 SnapshotExporter restarts from a prepared-only crash state
    And the managed projection MongoDB is paused
    Then consensus finality advances before and after projection MongoDB resumes
    When all validator nodes and OCOMP node-facing processes restart with preserved data
    Then the completed generation and exact vote replay remain identical

  @ocomp-delegated-signing @pfs-011-01
  # PFS-TEST-ID: PFS-011-01
  Scenario: Dedicated OCOMP keys submit a finalized vote without validator-key signing
    Given the canonical four-validator OCOMP Final devnet
    Then every OCOMP transaction signer is distinct and scoped only to the OCOMP role
    When an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the production OCOMP domains process that finalized JobIntent
    Then three matching validator domains atomically apply Lysis and create the Nod
    And all four OCOMP domains run their node-facing production roles

  @ocomp-capacity
  Scenario: A shard-cap-plus-one public population is completely processed
    Given a fresh four-validator OCOMP public capacity localnet
    When all 257 capacity owners submit one encrypted Tribute each
    Then all validators observe exactly 257 public Tributes for the capacity day
    When the committee logical clock reaches the public capacity processing time
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the production OCOMP domains process that finalized JobIntent
    Then three matching validator domains atomically apply Lysis and create the Nod
    And the certified generation contains exactly 257 Tribute and Nod records
    And validator 0 reconstructs that certified generation from canonical history
    And all four OCOMP domains run their node-facing production roles

  @metadosis-fresh-devnet
  Scenario: A fresh Metadosis day runs from Create through terminal OCOMP
    Given a fresh four-validator Metadosis capacity localnet at FORMING
    Then the fresh capacity day is created in FORMING by finalized block 1
    When the committee logical clock reaches the fresh capacity OFFERING window
    Then the same fresh capacity day advances through LOOKBACK to OFFERING
    When all 257 capacity owners submit one encrypted Tribute each
    Then all validators observe exactly 257 public Tributes for the capacity day
    When the committee logical clock reaches the fresh capacity processing time
    Then the same fresh capacity day advances through WAITING and READY
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the production OCOMP domains process that finalized JobIntent
    Then three matching validator domains atomically apply Lysis and create the Nod
    And the certified generation contains exactly 257 Tribute and Nod records
    And validator 0 reconstructs that certified generation from canonical history
    And all four OCOMP domains run their node-facing production roles
    And the fresh OCOMP domains retain their authenticated workers across the time changes

  @ocomp-final-capacity
  Scenario: The canonical Final devnet processes a shard-cap-plus-one population
    Given the canonical four-validator OCOMP Final devnet
    When all 257 capacity owners submit one encrypted Tribute each
    Then all validators observe exactly 257 public Tributes for the capacity day
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the production OCOMP domains process that finalized JobIntent
    Then three matching validator domains atomically apply Lysis and create the Nod
    And the certified generation contains exactly 257 Tribute and Nod records
    And validator 0 reconstructs that certified generation from canonical history
    And all four OCOMP domains run their node-facing production roles

  @ocomp-public-expiry
  # OCOMP-TEST-ID: OCM-PUB-003
  Scenario: Two timely votes cannot prevent exclusive-deadline expiry
    Given a fresh four-validator OCOMP short-window public measurement localnet
    When validators 2 and 3 OCOMP workers are stopped before the job
    And an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the production OCOMP domains process that finalized JobIntent
    And validator 2 prepares one valid vote without broadcasting it
    And the held validator vote is broadcast at the exclusive deadline
    Then the no-quorum job expires at its exclusive deadline without creating Nod

  @metadosis-failure-recovery
  Scenario: Exhausted OCOMP attempts fail one WWD without halting the chain
    Given a fresh four-validator OCOMP failure-recovery localnet
    When validators 2 and 3 OCOMP workers are stopped before the job
    And an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the production OCOMP domains process that finalized JobIntent
    Then the no-quorum job expires at its exclusive deadline without creating Nod
    And the exhausted no-quorum OCOMP day fails atomically
    When all validator nodes and OCOMP node-facing processes restart with preserved data
    Then the failed WWD and accounting remain identical after restart

  @ocomp-public-mutation
  # OCOMP-TEST-ID: OCM-PUB-002
  Scenario: A changed binding cannot mutate a non-quorum job or prevent exact recovery
    Given a fresh four-validator OCOMP public measurement localnet
    When validators 1, 2 and 3 OCOMP workers are stopped before the job
    And an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the production OCOMP domains process that finalized JobIntent
    And one valid vote is finalized and a changed-binding vote is submitted
    And the three stopped workers restart and form the remaining quorum
    Then three matching validator domains atomically apply Lysis and create the Nod
