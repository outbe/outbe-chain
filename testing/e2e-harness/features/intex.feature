@ocomp @tee @gramine-direct @min-validators-4
Feature: Intex from auction to Promis
  # An Intex has two halves of a life, and this feature owns both: the auction
  # that brings a series into existence, and everything the series is for once
  # it exists — qualifying, being settled by its holder, and burning into Promis.

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
  @intex-auction
  Scenario: A settled green day runs its auction through to a minted Intex
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
    And the cleared day mints the Intex on the target chain
    And the escrow settles the day and returns what the bids did not buy

  # Two series rather than one: the sweeps decide per (reference currency,
  # worldwide day), so a single series makes every group a group of one and
  # neither the group promotion nor the mark batching is exercised at all. The
  # two share a day and a reference currency, and differ in issuance currency.
  #
  # Time is seeded rather than lived through. A worldwide day sits in Forming
  # until its offering window closes, and stepping past that window on a day
  # OCOMP never formed is a fatal MissedOffering — forming one costs the whole
  # tribute path this scenario exists to avoid. So the days the Called sweep
  # reads are filled in and issuance is stamped behind them, exactly as this
  # module's own unit tests do. The sweep still walks its index, checks the
  # finalized watermark and counts the breach days itself.
  #
  # The rate, by contrast, is published through the real feeder: qualification
  # reads it with a freshness check that a seeded value would fail.
  @intex-lifecycle
  Scenario: Two Intex series qualify as one group, settle from both states, and burn into Promis
    Given a fresh four-validator OCOMP public capacity localnet
    When a local target chain is started
    And the intex venue is deployed on the target chain
    And the intex venue is wired
    When the intex engine is deployed on the committee chain
    Then the committee chain hosts the intex engine
    When a relay carries messages between the two chains
    And the settlement currency is registered on the committee chain
    Then holders may settle in that currency
    When two test Intex series sharing a reference currency are issued to a funded holder
    Then the holder holds issued units of both series on each chain
    When the day advances past the qualification period
    Then the controlled COEN USD quote is finalized through the real price feeder
    When the reference rate stands above the series floor
    Then both series qualify in one group decision
    When the holder settles part of their units
    Then those units move from issued to settled
    And the settlement payment lands in the reserve vault
    When the call trigger holds above the call price across the call window
    Then both series become Called
    When the holder settles the remaining units inside the notice period
    Then no issued units remain and every unit is settled
    When the holder mines Promis against their settled units
    Then the settled units are burned and Promis is minted
