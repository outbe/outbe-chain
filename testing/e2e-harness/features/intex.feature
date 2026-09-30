@ocomp @tee @sgx-no-attest @sudo @min-validators-4
Feature: Intex from auction to Promis
  # An Intex has two halves of a life, and this feature owns both: the auction
  # that brings a series into existence, and everything the series is for once
  # it exists - qualifying, being settled by its owner, and burning into Promis.

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
  # contract call - with no code at the address the node was built against, the
  # start is lost rather than retried into existence.
  #
  # The feeder publishes a live quote first: the auction's entry price is the last
  # closed day's VWAP, and the fixture's seeded pair is small enough that Lysis
  # would floor the monetary cost to zero without one.
  @intex-auction
  Scenario: A settled green day runs its auction through to an issued Intex
    Given a fresh four-validator OCOMP public capacity localnet
    When a local target chain is started
    And the intex venue is deployed on the target chain
    And the intex venue is wired
    Then the controlled COEN USD quote is finalized through the real price feeder
    When the intex engine is deployed on the committee chain
    Then the committee chain hosts the intex engine
    When a relay carries messages between the two chains
    When 33 capacity owners submit one encrypted Tribute each at no more than two per block
    Then all validators observe exactly 33 public Tributes for the capacity day
    When the committee logical clock reaches the public capacity processing time
    And the committee clock settles after the jump
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the production OCOMP domains process that finalized JobIntent
    Then three matching validator domains atomically apply Lysis and create the Nod
    And the auction for that day opens on every chain it named
    When two bidders commit their bids on every chain
    When those bidders reveal their bids once the venues are revealing
    Then the auction clears and the venue moves past its reveal window
    And the cleared day issues the Intex on every chain it reached
    And each escrow settles the day and returns what the bids did not buy

  # Four series of one day and one reference currency: the call sweep decides per
  # (reference currency, worldwide day), so a single series makes every group a
  # group of one and the mark batching is not exercised at all.
  #
  # Two series are paid for, one issued in MYR and one in EUR, each in two parts:
  # what is home once they qualify, and the rest once they are called and brought
  # home. The four payments take both rails and both currencies; the MYR ones
  # price off a closed pricing window, so the committee steps past the next whole
  # hour once the series are issued. On this localnet that hour is midnight: the
  # committee closes the day on its own feed, and every seeded day repeats that
  # close, because a target chain records a day's price once. The other two are
  # left to run out: one is settled in part and one never touched, so the sweep
  # has to return the load of the unrealized units alone from one and the whole
  # tirage from the other.
  #
  # Time is seeded rather than lived through. A worldwide day sits in Forming
  # until its offering window closes, and stepping past that window on a day
  # OCOMP never formed is a fatal MissedOffering - forming one costs the whole
  # tribute path this scenario exists to avoid. So the days the Called sweep
  # reads are filled in and issuance is stamped behind them, exactly as this
  # module's own unit tests do. The sweep still walks its index, checks the
  # finalized watermark and counts the breach days itself. Qualification reads a
  # closed day's VWAP as well, so that day is seeded the same way, and every
  # chain derives it from there.
  #
  # The two hops home also take the bridge's two routes: one series at a time
  # first, then both together, which is how an owner of several actually moves
  # them and which carries its own message encoding.
  @intex-lifecycle @myr-issuance
  Scenario: Four Intex series qualify as one group, two are paid on both rails in both currencies, two run out, and the paid ones end in COEN
    Given a fresh four-validator OCOMP public capacity localnet
    When a local target chain is started
    And the intex venue is deployed on the target chain
    And the intex venue is wired
    When the intex engine is deployed on the committee chain
    Then the committee chain hosts the intex engine
    When a relay carries messages between the two chains
    And the settlement currencies are registered on the committee chain
    Then owners may settle in each of them
    When four test Intex series sharing a reference currency are issued to the owner
    Then the owner holds issued units of every series on each chain
    And every Intex series reads Issued and carries its terms
    And no Intex series can be paid before it qualifies
    And the controlled COEN quotes are finalized through the real price feeder
    And the pricing window closes over those quotes
    When the reference rate stands above the Intex series floor
    Then every Intex series qualifies
    And every series card reads Qualified on both chains
    When the owner brings part of the target-chain units home
    Then an Intex series payment is refused for a stale snapshot, a foreign currency or another owner's note
    And an unpaid Intex series cannot be mined
    When a qualified Intex series is paid in USD by ERC20 and another in MYR by PayNote
    Then each payment settles exactly its quote into its currency's vault
    When the reference rate holds above the Intex series call price across the call window
    Then every unpaid Intex series becomes Called while the paid ones stay Settled
    When the owner brings the remaining units home to their own address in one batch
    And a called Intex series is paid in MYR by ERC20 and another in USD by PayNote
    Then each payment settles exactly its quote into its currency's vault
    When the owner settles part of one series they let run out
    And the call notice lapses on the unpaid Intex series
    Then the unpaid Intex series is forfeited and its load returns to the unallocated pool
    And the series left to run out read Expired on both chains
    And every paid Intex series stays Settled
    When the owners mine every paid Intex series
    Then each paid load lands in its owner's balance
    And a mined Intex series cannot be mined again
    When the owners redeem what they mined into COEN
    Then each owner's native COEN grows by exactly that load
