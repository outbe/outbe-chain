@tee @sgx-no-attest @sudo @min-validators-4 @fullnode-snapshot-anchor
Feature: FullNode consensus recovery from a native snapshot
  Scenario: Imported native history bounds consensus recovery on startup and restart
    Given a fresh four-validator snapshot localnet with short epochs
    When a signed native snapshot FullNode starts and restarts from its local committee anchor
