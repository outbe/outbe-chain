@tee @min-validators-4
Feature: Nod from issuance to Gratis and COEN
  # The Nod half of the shared lifecycle. Lysis over a certified generation is covered
  # by the OCOMP scenario; here the Nods are issued straight to five owners in one
  # bucket, so the phases every entity shares run on one chain and without OCOMP.
  #
  # Every Nod is issued in MYR against a USD reference. Two are paid while qualified
  # and two inside the call notice, one on each rail and in each currency; the MYR
  # payments price off a closed pricing window, so the committee steps past the next
  # whole hour once the quotes are finalized. The fifth Nod is left to forfeit.
  #
  # Time is seeded rather than lived through. Qualification and the call count only
  # closed days after the bucket's stamp, so the bucket is stamped behind the seeded
  # days. The seven-day call notice has no DEV profile and is closed by a test hook.
  @nod-lifecycle @myr-issuance
  Scenario: Five Nods in one bucket are paid on both rails in both currencies, one forfeits, and the paid ones end in COEN
    Given a fresh localnet with a 20-block voting window
    And the deploy account is funded on the committee chain
    When the settlement currencies are registered on the committee chain
    Then owners may settle in each of them
    And the controlled COEN quotes are finalized through the real price feeder
    And the pricing window closes over those quotes
    When five owners are issued Nods in one bucket
    Then every Nod reads Issued and carries its terms
    And no Nod can be paid before it qualifies
    And no Nod can be transferred
    When the reference rate stands above the Nod floor
    Then every Nod qualifies
    And a Nod payment is refused for a stale snapshot, a foreign currency or another owner's note
    And an unpaid Nod cannot be mined
    When a qualified Nod is paid in USD by ERC20 and another in MYR by PayNote
    Then each payment settles exactly its quote into its currency's vault
    When the reference rate holds above the Nod call price across the call window
    Then every unpaid Nod becomes Called while the paid ones stay Settled
    When a called Nod is paid in MYR by ERC20 and another in USD by PayNote
    Then each payment settles exactly its quote into its currency's vault
    When the call notice lapses on the unpaid Nod
    Then the unpaid Nod is forfeited and its load returns to the unallocated pool
    And every paid Nod stays Settled
    When the owners mine every paid Nod
    Then each paid load lands in its owner's balance
    And a mined Nod cannot be mined again
    When the owners redeem what they mined into COEN
    Then each owner's native COEN grows by exactly that load
