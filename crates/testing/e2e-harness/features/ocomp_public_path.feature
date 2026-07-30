@ocomp @tee @gramine-direct @min-validators-4
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
    Given the canonical four-validator OCOMP Final devnet
    When an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the validator supervisors submit results directly for that finalized JobIntent
    Then three matching validator domains atomically apply Lysis and create the Nod
    And all four OCOMP domains run their node-facing production roles
    When the completed full-result vote is retried and then mutated through public RPC
    Then the completed job and Nod generation are unchanged by both transactions

  @ocomp-delegated-signing @pfs-011-01
  # OCOMP-TEST-ID: PFS-011-01
  Scenario: Dedicated OCOMP keys submit a finalized vote without validator-key signing
    Given the canonical four-validator OCOMP Final devnet
    Then every OCOMP transaction signer is distinct and scoped only to the OCOMP role
    When an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the validator supervisors submit results directly for that finalized JobIntent
    Then three matching validator domains atomically apply Lysis and create the Nod
    And all four OCOMP domains run their node-facing production roles

  @ocomp-capacity
  Scenario: A shard-cap-plus-one public population is completely processed
    Given a fresh four-validator OCOMP public capacity localnet
    When all 257 capacity owners submit one encrypted Tribute each
    Then all validators observe exactly 257 public Tributes for the capacity day
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the validator supervisors submit results directly for that finalized JobIntent
    Then three matching validator domains atomically apply Lysis and create the Nod
    And the certified generation contains exactly 257 Tribute and Nod records
    And validator 0 reconstructs that certified generation from canonical history
    And all four OCOMP domains run their node-facing production roles

  @ocomp-final-capacity
  Scenario: The canonical Final devnet processes a shard-cap-plus-one population
    Given the canonical four-validator OCOMP Final devnet
    When all 257 capacity owners submit one encrypted Tribute each
    Then all validators observe exactly 257 public Tributes for the capacity day
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the validator supervisors submit results directly for that finalized JobIntent
    Then three matching validator domains atomically apply Lysis and create the Nod
    And the certified generation contains exactly 257 Tribute and Nod records
    And validator 0 reconstructs that certified generation from canonical history
    And all four OCOMP domains run their node-facing production roles

  @ocomp-public-expiry
  # OCOMP-TEST-ID: OCM-PUB-003
  Scenario: Two timely votes cannot prevent exclusive-deadline expiry
    Given the canonical four-validator OCOMP Final devnet
    When validators 2 and 3 OCOMP supervisors are stopped before the job
    And an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the validator supervisors submit results directly for that finalized JobIntent
    And validator 2 prepares one valid vote without broadcasting it
    And the held validator vote is broadcast at the exclusive deadline
    Then the no-quorum job expires at its exclusive deadline without creating Nod

  @ocomp-public-mutation
  # OCOMP-TEST-ID: OCM-PUB-002
  Scenario: A changed binding cannot mutate a non-quorum job or prevent exact recovery
    Given the canonical four-validator OCOMP Final devnet
    When validators 1, 2 and 3 OCOMP supervisors are stopped before the job
    And an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the validator supervisors submit results directly for that finalized JobIntent
    And one valid vote is finalized and a changed-binding vote is submitted
    And the three stopped supervisors restart and form the remaining quorum
    Then three matching validator domains atomically apply Lysis and create the Nod
