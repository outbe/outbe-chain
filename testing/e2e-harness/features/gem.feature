@tee @min-validators-4
Feature: Gem from a parked Intex to Promis
  # The merchant half of the Gem lifecycle, which nothing else covers: the
  # protocol's own reward gems are born Qualified and settled, so `Issued`,
  # the promotion on price, the Call and the two returns to the unallocated
  # pool have never run outside unit tests.
  #
  # One chain, and no OCOMP. Gems never leave the committee and the source
  # Intex is issued straight to the merchant, so this scenario needs neither a
  # second venue, nor a relay, nor a day settled out of Tributes.
  #
  # Time is seeded rather than lived through wherever the protocol allows it.
  # The call decision is day-granular, and a gem counts breach days from its own
  # issuance forward, so the gem that is called is stamped behind the seeded
  # days. The two waits that remain are real: a Call Notice has to lapse before
  # a forfeit, and a position has to outlive its validity, both shortened by the
  # DEV parameter profile this scenario runs against.
  @gem-lifecycle
  Scenario: A merchant parks an Intex, and its gems end mined, forfeited and unissued
    Given a fresh localnet with a 20-block voting window
    When the intex engine is deployed on the committee chain
    Then the committee chain hosts the intex engine
    When the settlement currency is registered on the committee chain
    Then holders may settle in that currency
    And the controlled COEN USD quote is finalized through the real price feeder
    When a test Intex series is issued to a funded merchant
    And the merchant parks part of their units into a gem position
    Then the position holds the parked capacity and the units are burned
    When the merchant issues two gems from the position, leaving capacity unissued
    Then both gems read Issued and carry the position's terms
    When the reference rate stands above the gem floor
    Then both gems qualify
    When the merchant settles the first gem and mines its Promis
    Then that gem is burned and its load lands in the merchant's Promis
    When the call trigger holds above the second gem's call price across its window
    Then that gem becomes Called
    And it is forfeited and its load returns to the unallocated pool
    When the position's validity runs out
    Then the position returns its unissued capacity to the same pool
