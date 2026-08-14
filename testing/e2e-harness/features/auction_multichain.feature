@ocomp @tee @gramine-direct @min-validators-4
Feature: Multichain auction from a settled day
  # Applying the day's OCOMP result hands Desis its brief, and a later schedule
  # tick dispatches AUCTION_STAGE_START to the origin router, which routes it to
  # every registered target. The committee is registered as its own target, so
  # the whole path runs on one chain: a day whose result lands must end with an
  # auction open on the venue.
  #
  # The public capacity fixture is what makes this reachable at all: it seeds a
  # rising pair of Oracle rates, and only a day whose rate rose is green. A day
  # with no prior rate is red by definition, briefs no supply, and can never
  # open an auction.
  #
  # The venue is deployed before the day settles because the dispatch is a plain
  # contract call — with no code at the address the node was built against, the
  # start is lost rather than retried into existence.

  @auction-start
  Scenario: A settled green day opens its auction on the target chain
    Given a fresh four-validator OCOMP public capacity localnet
    When the intex engine is deployed on the committee chain
    Then the committee chain hosts the intex engine
    When 33 capacity owners submit one encrypted Tribute each at no more than two per block
    Then all validators observe exactly 33 public Tributes for the capacity day
    When the committee logical clock reaches the public capacity processing time
    And the committee clock settles after the jump
    Then Metadosis creates one finalized JobIntent from that public Tribute
    And the auction for that day opens on the target chain
    When two bidders commit their bids
    And the production OCOMP domains process that finalized JobIntent
    Then three matching validator domains atomically apply Lysis and create the Nod
    When those bidders reveal their bids once the venue is revealing
    Then the auction clears and the venue moves past its reveal window
