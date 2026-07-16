@min-validators-4
Feature: Committee OIP/GIP via vote
  # Mirrors update_operator.feature for the governance VoteTarget: operators
  # propose OIP then GIP via IVote; after quorum + deadline both materialize as
  # Approved governance records. Two proposers are required because
  # MAX_PENDING_PROPOSALS_PER_VALIDATOR is 1.
  #
  # The voting window must outlast sequential propose + cast RPC round-trips
  # (same rationale as update_operator.feature).

  Scenario: OIP and GIP are proposed, approved, and materialized as Approved
    Given a fresh localnet with a 20-block voting window
    And the committee has reached a usable height
    When operator "validator-0" proposes an OIP with text "e2e oip body"
    Then proposal 1 is pending and targets the governance module with kind "oip"
    When operator "validator-1" proposes a GIP with text "e2e gip body"
    Then proposal 2 is pending and targets the governance module with kind "gip"
    When validators "validator-0,validator-1,validator-2" cast yes votes on proposal 1
    And validators "validator-0,validator-1,validator-2" cast yes votes on proposal 2
    Then proposal 1 is still pending with 3 yes votes
    And proposal 2 is still pending with 3 yes votes
    When the committee passes the vote deadline
    Then proposal 1 is approved
    And proposal 2 is approved
    And OIP 1 is Approved with text "e2e oip body" authored by "validator-0"
    And GIP 1 is Approved with text "e2e gip body" authored by "validator-1"
    And the committee nodes agree on the state root
