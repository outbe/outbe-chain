@ocomp @tee @gramine-direct @min-validators-4
Feature: Certified contributor payout authority
  # The Lysis activation frame installs the Nod generation and the certified
  # contributor authority together, so applying one public result must leave a
  # contributor root every validator agrees on. The payout round itself stays
  # closed until auction proceeds arrive, which is what makes the authority
  # readable but not yet payable at this point.
  #
  # Driven from a fresh capacity localnet rather than the canonical Final
  # devnet: the day is generated in-run, so this scenario owns its whole
  # population and depends on no pinned artifact.

  @contributor-authority
  Scenario: A fresh day's Lysis activation installs the certified contributor authority
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
    And the certified contributor authority for that day is identical on every validator
    And that day has no open contributor payout round before proceeds arrive
    When the day's auction proceeds arrive from one chain
    Then every certified contributor is paid their share
