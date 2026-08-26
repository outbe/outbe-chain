# Off-chain PoC process and artifact topology

Status: **UPDATED ON 2026-08-26**

This document is the current process/transport decision for OCOMP.

## 1. Decision

One validator administrative domain contains three process roles. The Supervisor
is an embedded `outbe-chain` ExEx component, not a fourth process:

```text
public finalized chain RPC
       | blocks, receipts, logs, eth_call, eth_getProof
       v
+-------------------+       filesystem CAS       +-----------------------+
| SnapshotExporter  | --------------------------> | outbe-chain node      |
| public RPC reader |                             | embedded Supervisor   |
+-------------------+                             | scheduler + voter     |
                                                  +-----------+-----------+
                                                              |
                               Axum: register only             |
                     +----------------------------------------+
                     |                                        |
                     | ZeroMQ ROUTER/DEALER over loopback TCP |
                     | work, accepted, heartbeat, cancel, done|
                     v                                        v
               +------------+  +------------+  ...      +------------+
               | Worker 0   |  | Worker 1   |           | Worker 3   |
               | Salvo obs. |  | Salvo obs. |           | Salvo obs. |
               +------------+  +------------+           +------------+
```

The node owns the loopback OCOMP registration/status HTTP endpoint and ZeroMQ
router. Its embedded Supervisor discovers finalized jobs directly from the ExEx
stream; SnapshotExporter reconstructs finalized authority using the node's
existing public RPC. The embedded Supervisor prepares, signs and submits the
normal validator-associated result-vote transaction over that RPC.

There is no CAS daemon. Each validator domain owns one bounded filesystem CAS;
correctness is re-established from canonical bytes, digests, roots and the
finalized public-chain proofs.

## 2. HTTP surfaces

Supervisor uses Axum only for registration and observability:

| Endpoint | Purpose |
|---|---|
| `GET /health` | Supervisor liveness |
| `GET /v1/status` | registry, connected-worker and queue status |
| `POST /v1/workers/register` | bind one process nonce to a worker identity and return the ZeroMQ endpoint |

Worker does not poll Supervisor over HTTP. A successful registration returns
the current registry generation, stable worker ID and loopback ZeroMQ TCP
endpoint.

Every Worker retains a small local Salvo server exclusively for observability:

| Endpoint | Purpose |
|---|---|
| `GET /healthz` | Worker process liveness |
| `GET /status` | registering/idle/working/cancelling/disconnected state and active lease |

Worker Salvo endpoints do not accept work, results, heartbeats or cancellation.

## 3. Work transport

Supervisor owns one ZeroMQ `ROUTER`; every registered Worker connects one
`DEALER` using its assigned worker ID as routing identity. The carrier is
loopback TCP. Required work is not sent through PUB/SUB because delivery and
acknowledgement are part of lease safety.

Supervisor commands are:

- `Work`: exact canonical `RunUnitV1`, lease identity and registry generation;
- `Cancel`: exact active lease identity and registry generation.

Worker events are:

- `Ready`;
- `Accepted`;
- `Heartbeat`;
- `Completed` with canonical `UnitFinishedV1`.

The registry generation and exact worker/lease/unit binding reject stale or
cross-worker messages. Dispatch is asynchronous per Worker: a stalled HTTP
client, disconnected Worker or long computation cannot block other Workers or
the Supervisor's RPC discovery loop.

## 4. Worker population and lifecycle

One embedded Supervisor accepts between one and four external Worker processes. Worker ordinals
`0..3` derive distinct process nonces and therefore distinct worker IDs. The
generated launch bundle starts the configured population and verifies the
connected count through Supervisor status. It has systemd units for the external
SnapshotExporter and Worker; the Supervisor stays inside the node process.

Registration is idempotent for one live process nonce. Re-registration or lease
expiry invalidates the prior connection, requeues unfinished work with a new
delivery attempt and rejects stale completion. Explicit cancellation marks the
active execution token; the Worker checks it at bounded execution checkpoints
and never publishes a cancelled result.

The E2E port allocator reserves, per validator, consecutive loopback TCP ports
for Supervisor registration, Supervisor ZeroMQ and four Worker Salvo endpoints.
This prevents one validator's message endpoint from overlapping the next
validator's RPC block.

## 5. Authority and capability boundaries

- SnapshotExporter can only obtain authority through finalized public RPC
  receipts, exact-block calls/proofs and verified job intent bindings.
- Supervisor never receives a generic signing request. It signs only the fixed
  result-vote transaction with its validator-associated OCOMP signer.
- Worker receives canonical unit bytes and content-addressed object references;
  it receives no node database, validator key, arbitrary executable or path.
- Axum, Salvo and ZeroMQ messages are operational transport only. They cannot
  reinterpret OCB1 bytes or create chain authority.

The old `OcompControlV1`, Hello/session and node-control payloads have been
removed. The remaining `local_control` module is transport-free and contains
only process identity and service-user helpers.

## 6. Failure semantics

| Failure | Local outcome | Chain outcome |
|---|---|---|
| node or embedded Supervisor unavailable | no scheduling or vote; journals resume with the node | the node service must recover |
| SnapshotExporter absent | no new authenticated manifest | blocks/finality continue |
| Worker disconnect/crash | active lease expires, is cancelled and requeued | unchanged |
| fifth Worker | registration rejected by the bounded registry | unchanged |
| malformed/stale ZeroMQ message | reject message/lease binding | unchanged |
| stalled HTTP connection | isolated async request; other routes and Workers continue | unchanged |
| CAS corruption/quota exhaustion | verification or publication fails; validator abstains | unchanged |
| vote submission delay | retry exact prepared transaction under the submission journal | unchanged until inclusion/deadline |

No OCOMP failure invokes synchronous on-chain Lysis or requests node shutdown.

## 7. Verification surfaces

The implementation is closed by:

- registry capacity and stale-lease unit tests;
- a stalled-registration-connection concurrency test;
- Worker Salvo `/healthz` and `/status` tests while computation is active;
- real Worker process dispatch/completion and cancellation tests;
- multi-worker identity and launcher tests;
- an ignored, compile-checked public-RPC integration test covering discovery,
  finalized proofs, receipt projection, WWD reconstruction, durable export,
  restart idempotency and identity mismatch rejection;
- repository inventory proving OCOMP runtime has no Unix listener/stream/path.

The ignored public-RPC test requires a finalized OCOMP request and a
transaction-capable MongoDB deployment and is intentionally run on the dedicated
integration machine, not as part of ordinary local validation.
