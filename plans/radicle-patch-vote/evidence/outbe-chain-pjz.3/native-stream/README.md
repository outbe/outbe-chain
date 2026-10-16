# Intermediate stream run with a real lint failure

Source 6c9c71082: native suites passed669 tests and root consumers/10 sidecar tests passed. The native runner then stopped at Clippy exit101 on unchecked JSON indexing in the newly added CLI integration test. Format and CLI commands later in that runner were NOT RUN. No all-green check or P2 acceptance is claimed here. Exact executed commands/times/exits/log hashes are preserved.

The indexing was corrected in source6c19aaaf514b7d5bd77c3de3559185459eaf425f without a lint suppression. Complete reruns and proof are in ../native-stream-r2/.

`diagnostic-before.log` is an earlier development reproduction on the preceding source plus an uncommitted regression assertion: the real fetched BLS Review was readable as a Patch but PatchStream failed NativeContextRequired. It is a diagnostic, not a clean-SHA acceptance run, and does not prove D1 or unauthorized patch acceptance.
