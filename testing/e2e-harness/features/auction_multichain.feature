@ocomp @tee @gramine-direct @min-validators-4
Feature: Multichain auction from a settled day
  # Applying the day's OCOMP result hands Desis its brief, and a later schedule
  # tick dispatches AUCTION_STAGE_START to the origin router, which routes it to
  # every registered target. The committee is registered as its own target, so
  # the whole path runs on one chain: a day whose result lands must end with an
  # auction open on the venue.
  #
  # The venue is deployed before the day settles because the dispatch is a plain
  # contract call — with no code at the address the node was built against, the
  # start is lost rather than retried into existence.

  @auction-start
  Scenario: A settled day opens its auction on the target chain
    Given a fresh four-validator Metadosis capacity localnet at FORMING
    When the intex engine is deployed on the committee chain
    Then the committee chain hosts the intex engine
    And the fresh capacity day is created in FORMING by finalized block 1
    When the committee logical clock reaches the fresh capacity OFFERING window
    Then the same fresh capacity day advances through LOOKBACK to OFFERING
    When all 257 capacity owners submit one encrypted Tribute each
    Then all validators observe exactly 257 public Tributes for the capacity day
    When the committee logical clock reaches the fresh capacity processing time
    Then the same fresh capacity day advances through WAITING and READY
    And Metadosis creates one finalized JobIntent from that public Tribute
    When the production OCOMP domains process that finalized JobIntent
    Then three matching validator domains atomically apply Lysis and create the Nod
    And the auction for that day opens on the target chain
