# Storage ownership lifecycle

`StorageProvider` selects the configured adapter and constructs `OpenedStorage`.
The ownership module accepts reader/writer capabilities and an injected
`StorageLifecycle`; it does not select or enumerate physical backends.
An additional adapter implements activation and idempotent, acknowledged close.

Every issued reader and writer holds the same session. Dropping the ownership
guard cannot release a writer lease while a capability is still alive.
`StorageCompletion` observes teardown without retaining the session itself.

Opening exposes inspection reads and an inactive ordinary writer. The scoped
`ownership.preflight` callback receives a temporary writer for managed-state
initialization and the transaction-capability probe. Ordinary writes remain
blocked. Bootstrap handles are revoked when the callback finishes, including
unwinding; revocation waits for already-running bootstrap operations.

The node checks chain identity and the canonical checkpoint. Only then does it
call `ownership.activate`. Activation replays a prepared journal through the
injected adapter and enables ordinary writes after success. Failed activation
keeps ordinary writes blocked and permits retry. Successful activation is
idempotent. Blockchain validation and readiness remain responsibilities of the
node, not the physical storage adapter.

After the final session capability is dropped, a cleanup worker releases raw
reader/writer references and closes adapter ownership. Rocks waits for every
registered primary destructor, including retired partitions and independent
partition readers. Mongo stops and joins the lease renewer before performing an
owner-qualified, acknowledged lease deletion. Cleanup errors remain observable;
the worker retries with a one-second internal interval. Raw Mongo lease `Drop`
retains its best-effort fallback when managed cleanup has not completed.

`completion.wait_timeout` returns `Timeout` or a retained cleanup error rather
than claiming success. A timeout does not revoke handles or cancel cleanup.
The node registers completion before each startup preflight attempt and waits
in five-second intervals after execution teardown until all registrations have
completed successfully, including failed preparation attempts.

Tests exercise public session capabilities, adapter lifecycle injection, real
Rocks partition-reader/recovery behavior, real Mongo lease-release failures,
and node checkpoint/startup/shutdown interfaces. The Mongo cleanup fault test
requires an isolated replica with `enableTestCommands`; ordinary Mongo tests
only require the normal isolated replica URI.
