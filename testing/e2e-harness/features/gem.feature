@tee @min-validators-4
Feature: Gem from a parked Intex to Promis and COEN
  # The merchant half of the Gem lifecycle, which nothing else covers: the
  # protocol's own reward Gems are born Qualified and settled, so `Issued`,
  # the promotion on price, the Call and the two returns to the unallocated
  # pool have never run outside unit tests.
  #
  # One chain, and no OCOMP. Gems never leave the committee and the source
  # Intex is issued straight to the merchant, so this scenario needs neither a
  # second venue, nor a relay, nor a day settled out of Tributes.
  #
  # The source Intex is issued in MYR against a USD reference, and the merchant
  # issues one Gem to each of five owners. Two are paid while qualified and two
  # inside the call notice, one on each rail and in each currency; the MYR
  # payments price off a closed pricing window, so the committee steps past the
  # next whole hour before the position is parked, whose validity the step would
  # otherwise spend. The fifth Gem is left to forfeit.
  #
  # Time is seeded rather than lived through wherever the protocol allows it.
  # Qualification and the call count only days a Gem held in full, so the Gems
  # are stamped behind the seeded days. The two waits that remain are real: a
  # Call Notice has to lapse before a forfeit, and a position has to outlive its
  # validity, both shortened by the DEV parameter profile this scenario runs
  # against.
  @gem-lifecycle @myr-issuance
  Scenario: Five Gems from one position are paid on both rails in both currencies, one forfeits, and the paid ones end in COEN
    Given a fresh localnet with a 20-block voting window
    When the intex engine is deployed on the committee chain
    Then the committee chain hosts the intex engine
    When the settlement currencies are registered on the committee chain
    Then owners may settle in each of them
    And the controlled COEN quotes are finalized through the real price feeder
    And the pricing window closes over those quotes
    When a test Intex series is issued to the merchant in MYR against USD
    And the merchant parks part of their units into a Gem position
    Then the position holds the parked capacity and the units are burned
    When the merchant issues a Gem to each of five owners, leaving capacity unissued
    Then every Gem reads Issued and carries its terms
    And no Gem can be paid before it qualifies
    And no Gem can be transferred
    When the reference rate stands above the Gem floor
    Then every Gem qualifies
    And a Gem payment is refused for a stale snapshot, a foreign currency or another owner's note
    And an unpaid Gem cannot be mined
    When a qualified Gem is paid in USD by ERC20 and another in MYR by PayNote
    Then each payment settles exactly its quote into its currency's vault
    When the reference rate holds above the Gem call price across the call window
    Then every unpaid Gem becomes Called while what was paid stays Settled
    When a called Gem is paid in MYR by ERC20 and another in USD by PayNote
    Then each payment settles exactly its quote into its currency's vault
    When the call notice lapses on the unpaid Gem
    Then the unpaid Gem is forfeited and its unpaid load returns to the unallocated pool
    And every paid Gem stays Settled
    When the owners mine every paid Gem
    Then each paid load lands in its owner's balance
    And a mined Gem cannot be mined again
    When the owners redeem what they mined into COEN
    Then each owner's native COEN grows by exactly that load
    When the position's validity runs out
    Then the position returns its unissued capacity to the same pool

  @ocomp @price-oracle @paynote-main
  Scenario: Ten old PayNotes fully settle ten GEMs
    Given a fresh localnet with a 20-block voting window
    When the intex engine is deployed on the committee chain
    Then the committee chain hosts the intex engine
    When the settlement currency is registered on the committee chain
    Then owners may settle in that currency
    And the controlled COEN USD quote is finalized through the real price feeder
    Then 10 PayNotes deposited before any spend fully settle 10 GEMs on every validator

  @ocomp @price-oracle @paynote-capacity
  Scenario: One thousand old PayNotes fully settle one thousand GEMs
    Given a fresh localnet with a 20-block voting window
    When the intex engine is deployed on the committee chain
    Then the committee chain hosts the intex engine
    When the settlement currency is registered on the committee chain
    Then owners may settle in that currency
    And the controlled COEN USD quote is finalized through the real price feeder
    Then 1000 PayNotes deposited before any spend fully settle 1000 GEMs on every validator
