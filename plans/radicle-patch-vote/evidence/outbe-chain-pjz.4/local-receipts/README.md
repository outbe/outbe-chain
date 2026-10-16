# Intermediate P2/P3: protected local finalized-receipt channel

Frozen corrected source: `301b4763aa2025c8e02b19819b05dbc49ce342b3`.
Component base: `9d3373feb206f848023397e2203e747bbd0bc8cb`.
checks.json records exact source/spec hashes, all changed paths, actual argv/cwd/
environment/toolchain/platform, start/end times, exits and log hashes. All 60 adapter
lib tests, seven consumer all-target checks, adapter plus node all-target Clippy
with `-D warnings`, and formatting of all eight changed Rust files passed. Tracked
source under bin/crates/scripts/vendor and Cargo files was clean after checks.

| Required fragment | Production source | Behavioral evidence in full-libs.log |
| --- | --- | --- |
| Trusted local chain source, no peer-selected authority or archived fallback | manager/types.rs::RepositoryReceiptReader; snapshot.rs implementation for RethSnapshotReader | local_receipt_reader_samples_own_current_finality_without_archived_fallback reads current canonical finalized head, then rejects corrupt/missing/newer state and absent finality while the old valid state remains present |
| Protected native-local transport | integration/local.rs::private_parent, private_socket, peer, LocalReceiptServer::bind and LocalReceiptClient::read | actual_local_socket_returns_bound_receipts_without_fallback uses real same-UID Unix sockets; private_permissions_symlinks_live_owners_and_stale_sockets_are_checked rejects public parent/socket modes and symlinks without changing their targets or permissions |
| Independent socket ownership and restart cleanup | SocketLease, OS File::try_lock, nonblocking probe | second live owner fails; an active listener without this lock is not replaced; a stale socket is recovered; graceful stop removes its own socket and allows reopen; dropping an old owner leaves a replacement inode untouched |
| Fixed bounded request/response frames | ORL1 framing, read_frame, decode_response | malformed_oversized_and_arbitrary_signing_requests_never_reach_reader rejects oversized/unknown/truncated request shapes without any state read; response_decoder_checks_exact_request_chain_and_receipt_integrity rejects wrong sequence, framing, zero observation hash, future acceptance height and corrupted receipt/body |
| Deadline and bounded blocking work | run timeout, MAX_CONNECTIONS=32, MAX_READERS=4 with retained blocking permits | client_deadline_covers_an_authenticated_but_unresponsive_peer exercises actual connected socket plus virtual Tokio deadline; blocking_state_reads_are_bounded_without_an_unbounded_wait_queue observes four blocked readers, rejects a fifth, advances past client deadlines and proves no extra reader starts until the old work completes |
| Node lifecycle and opt-in scope | bin/outbe-chain/src/launch/node/radicle.rs | source plus actual node all-target compilation/Clippy: governments-enabled validator observer owns the server, stops it before manager/endpoint drain and records service failures; disabled genesis keeps no listener. Full live-node lifecycle is not exercised here |

## Exact transport boundary

The socket path is the configured native Heartwood control path with `.outbe`
appended. Its parent must already be owned by the effective user with mode 0700;
the socket and ownership lock use mode 0600. Both peers check the Unix peer UID.
The server uses only its own validating-node RepositoryReceiptReader. The client
requires its expected chain/genesis and checks the returned sequence, body hash,
accepted height and finalized observation. A sealed FinalizedPublicationReceipt
is created only after the authenticated exchange and these checks.

One request per connection: u32be length, then `ORL1 || sequence:u64be` (12 bytes).
Response: u32be length then `ORL1 || status:u8`. Status 0 means unavailable, status
1 means absent, both exactly five bytes. Status 2 appends finalized height:u64be,
finalized hash:32, accepted height:u64be, receipt hash:32 and exact OPB1 body:477,
for 562 payload bytes. Unknown/trailing/oversized frame forms reject. Each async
exchange has a five-second deadline; at most 32 connection tasks and four actual
blocking state reads are retained. Reader saturation returns unavailable, without
queuing another blocking read. Current Reth source performs canonical/finality
validation; no client-supplied block height/hash selects the state.

No secret Share, partial signature, signing message, arbitrary namespace, Vote
interpreter or new consensus wire format is carried here. Rustix was already a
workspace dependency; this component uses its safe process/net APIs and no unsafe
code. Cargo.lock only adds that existing dependency to outbe-radicle.

## Evidence limits and remaining work

Socket tests use real filesystem/listeners/peer credential calls and fixture receipt
readers. The Reth adapter has a separate real production-path state-provider fixture
test. This is not a launched chain/sidecar pair or a cryptographic checkpoint E2E.
The fake transport body is not claimed as a valid BLS bootstrap. Wrong-UID rejection
is source-backed; a second OS user/Linux container was not launched. Connection cap
32 is source-backed; the exercised saturation is the actual four-reader pool.

The client is ready for collector integration; sidecar startup/collector does not
consume it yet. Typed current material/signing requests, bootstrap identity grants,
latest snapshot promotion/CAS, persisted policy rebuild, full collector journal/
distributed collection, automatic governments, Merge and production E2E remain
required. No task closure or acceptance review PASS is claimed. Native Heartwood
and consensus signing source were unchanged.

failed-parent-fixture retains the initial fixture failure plus its diagnostic
snapshot: tempfile parents did not have mode0700; the deadline test waited on
accept after its client refused that parent. The confirmed own test PID was stopped
with SIGINT (cargo exit101). The snapshot includes a reader test added while that
targeted run was in progress and is explicitly not exact whole-build provenance.
failed-macos-flags records source82e21f9eb compile exit101: the selected Rustix
SocketFlags constants are absent on Darwin. Source301b4763a replaces them with
safe portable std/rustix calls. Actual start/end timestamps of those earlier probes
were not captured; their metadata labels them diagnostic, never acceptance proof.
The corrected frozen checks above have complete timing/provenance and all passed.
