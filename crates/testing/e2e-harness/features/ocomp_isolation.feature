@ocomp @ocomp-isolation @tee @gramine-direct @sudo @min-validators-4
Feature: OCOMP service-manager isolation
  # OCOMP-TEST-ID: OCM-ISO-001
  Scenario: Four validator domains retain node liveness under bounded compute faults
    Given four OCOMP domains are running through the production systemd profile
    When an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    And every validator serves the same independently verified compressed tribute
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the validator supervisors submit results directly for that finalized JobIntent
    Then three matching validator domains atomically apply Lysis and create the Nod
    Then every domain has distinct node supervisor exporter and worker process boundaries
    And the runtime inventory proves role UIDs cgroup v2 mount isolation quotas and UDS credentials
    When one supervisor CAS path and one socket-activated worker are failed
    Then both faults stay inside their OCOMP domains and all four nodes continue finalizing
    And worker concurrency and aggregate resource limits remain within the PoC profile
