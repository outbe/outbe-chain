@tee @min-validators-4
Feature: Nod from issuance to settlement and forfeit
  # The call half of the Nod lifecycle, which the OCOMP scenario never reaches:
  # it pays qualified Nods, so the call, a payment inside the notice and the
  # forfeit of an unpaid Nod have never run outside unit tests.
  #
  # One chain, and no OCOMP. Lysis over a certified generation is covered by
  # the OCOMP scenario, so these Nods are issued straight to their owners.
  #
  # Time is seeded rather than lived through. Qualification and the call are
  # day-granular and count only days a bucket held in full, so the bucket is
  # stamped behind the seeded days. The seven-day call notice is skipped by
  # moving the bucket's call back, after the payment inside it has landed.
  @nod-lifecycle
  Scenario: Two Nods in one bucket are called, one is paid inside the notice and the other forfeited
    Given a fresh localnet with a 20-block voting window
    When the settlement currency is registered on the committee chain
    Then owners may settle in that currency
    And the controlled COEN USD quote is finalized through the real price feeder
    When two owners are issued Nods in one bucket
    Then both Nods read Issued and carry the bucket's call terms
    When the reference rate stands above the Nod floor
    Then both Nods qualify
    When the reference rate holds above the call price across the call window
    Then the bucket is Called with its notice running
    When the first Nod is paid inside the notice
    Then that Nod is Settled while the other stays Called
    When the call notice lapses
    Then the unpaid Nod is forfeited and burned while the paid one stays Settled
