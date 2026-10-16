# User clarification and withdrawn D1 approval request

2026-10-15. The user explicitly confirmed:

- The network-created repository has only the network BLS authority. No user delegation feature is authorized.
- Only network Vote by its eligible current ValidatorSet decides acceptance of an exact patch revision.
- An accepted patch stays accepted; validator/material changes do not revoke accepted history.

The earlier approval request is withdrawn pending concrete semantic evidence. This does not authorize option A. The original proposal, snapshot and independent reports remain historical evidence and are not edited. No D1 implementation has been made.

The original counterexample establishes a dependency-load admission gap. It does not establish unauthorized patch acceptance, a Vote bypass, a network-signature verification bypass, or an old-shares attack. P3 already requires current-material signer and partial verification. Do not present old shares or normal user delegation as D1 justification.

An additional diagnostic scenario was started to examine the production patch evaluator; it currently has compile errors in the diagnostic code. No successful semantic result is claimed. Any adversarial identity input is a negative test, never an authorized product feature. Do not introduce delegation or revocation into implementation.

The user requested resumption. get_goal returned blocked; the exposed goal API has no resume operation. The implementation and acceptance gates remain unchanged.
