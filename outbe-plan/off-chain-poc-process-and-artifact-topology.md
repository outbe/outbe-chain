# Off-chain PoC process and artifact topology

Status: **RESOLVED — decision ticket #6**

This decision fixes the smallest local topology that demonstrates real process,
resource and artifact failure boundaries for the PoC. It is subordinate to:

- the [PoC scope](../off-chain-poc.md);
- the [protocol-byte freeze](off-chain-poc-protocol-freeze.md);
- the [finalized input/export decision](off-chain-poc-finalized-input-export.md);
- `ADR-S-OCM-001` and `ADR-S-OCM-002`.

It does not add a production launch broker, remote workers, a generic program
runtime or a second off-chain program.

## 1. Decision in one view

One validator administrative domain is:

```text
                         public chain traffic
                                  |
                                  v
  +---------------------------------------------------------------+
  | outbe-chain.service                         UID: outbe          |
  | consensus/finality, Job FSM, OCOMP signer and sign-once       |
  | /run/outbe/ocomp-control.sock                                 |
  +--------------------------+------------------------------------+
                             | bounded OcompControlV1 / SO_PEERCRED
                 +-----------+------------------+
                 |                              |
                 v                              v
  +-----------------------------+  +------------------------------+
  | snapshot-exporter.service   |  | ocomp-supervisor.service     |
  | UID: outbe-ocomp-export     |  | UID: outbe-ocomp-supervisor  |
  | CE exact RO lease + Mongo RO|  | cursor/plan/schedule/reduce   |
  +---------------+-------------+  +---------------+--------------+
                  | verified objects               | CAS objects RO/RW
                  +---------------+----------------+
                                  v
  +---------------------------------------------------------------+
  | /var/lib/outbe-ocomp/cas-v1     separate mount/project quota  |
  | immutable-by-convention digest-addressed objects              |
  | correctness is re-established from bytes, digests and roots   |
  +--------------------------+------------------------------------+
                             | CAS bind-mounted read-only
                             v
  +---------------------------------------------------------------+
  | outbe-ocomp-worker.socket                                     |
  | AF_UNIX stream, Accept=yes, MaxConnections=4                  |
  | one accepted connection -> one worker@.service process        |
  +--------------------------+------------------------------------+
                             |
                             v
  +---------------------------------------------------------------+
  | outbe-ocomp-worker@.service         UID: outbe-ocomp-worker    |
  | exactly one UnitId, CAS RO, bounded inbox/scratch RW, no net   |
  +---------------------------------------------------------------+

                outside all validator administrative domains
  +---------------------------------------------------------------+
  | outbe-ocomp relay                                             |
  | trivial replaceable HTTP collector and ordinary RPC submitter  |
  | no consensus key and no OCOMP result key                      |
  +---------------------------------------------------------------+
```

There is no CAS daemon. The CAS failure domain is a distinct quota-controlled
filesystem volume. Adding a daemon would add a process, protocol and recovery
surface without adding authority or correctness.

## 2. Smallest code/package shape

Exactly two new workspace packages are justified:

| Package | Contents | Why it is separate |
|---|---|---|
| `crates/system/ocomp-protocol` / `outbe-ocomp-protocol` | OCB1 schemas/codecs, IDs/hashes, bounded local control frames and pure verifiers | both the node and compute binary need the exact bytes; it must not depend on node, databases, process launch, HTTP or Lysis storage |
| `bin/outbe-ocomp` / `outbe-ocomp` | a library plus one executable with `supervisor`, `snapshot-exporter`, `worker` and `relay` subcommands | all four roles share codecs, CAS verification and diagnostics, while service UIDs/mounts/sockets—not executable filenames—enforce authority |

Node-owned `OcompControl`, retention and attestation code remains in the
existing node/system ownership selected by later lifecycle tasks. Pure Lysis V1
semantics remains in `crates/core/lysis`; it is not copied into a process crate.
The E2E process owner remains `crates/testing/e2e-harness`.

Using one executable for four roles is not an authority shortcut:

- a worker UID cannot connect to the node control socket;
- it sees the CAS read-only and no node/key/database paths;
- an exporter UID cannot call supervisor or attestation methods;
- a supervisor UID cannot read CE/Mongo credentials or the OCOMP key;
- relay mode receives no validator-domain files or sockets.

Invoking another subcommand therefore does not grant that role's capabilities.
Startup fails closed if the expected UID, inherited file descriptors, peer
credentials, mount access or configured role does not match.

Not selected:

- one crate per process;
- a generic `ProgramRegistry`, `TaskAdapter` or executor plugin API;
- linking exporter/worker code into `outbe-chain`;
- a separate CAS service;
- a production worker launch broker;
- direct `Command` execution of caller-selected paths.

## 3. Process ownership and lifecycle

### 3.1 Fixed identities

The deployment declares stable service identities:

| Role | User | Supplementary access |
|---|---|---|
| node | existing `outbe` | node state, node OCOMP state/key, OcompControl socket owner |
| exporter | `outbe-ocomp-export` | node-control client group, CE read-only group, compute-CAS writer group |
| supervisor | `outbe-ocomp-supervisor` | node-control client group, worker-socket group, compute-CAS writer group |
| worker | `outbe-ocomp-worker` | no node-control group; CAS is exposed by a read-only bind mount |
| relay | deployment-local unprivileged identity outside validator domains | public HTTP/RPC only |

Membership in the shared node-control filesystem group only permits a
connection. The node uses Linux `SO_PEERCRED` and a fixed UID-to-role table
before granting a method capability. Linux captures peer PID/UID/GID for a
connected AF_UNIX stream socket; pathname permissions alone are not treated as
authentication. See [`unix(7)`](https://man7.org/linux/man-pages/man7/unix.7.html).

The OCOMP result private key and sign-once journal are node-readable only.
Mongo read credentials are exporter-readable only and are loaded as service
credentials, not environment variables or command-line arguments.

### 3.2 systemd graph

The planned unit set is:

```text
outbe-validator-stack.target
  Wants=outbe-validator.service
  Wants=outbe-ocomp.target

outbe-ocomp.target
  Wants=outbe-ocomp-snapshot-exporter.service
  Wants=outbe-ocomp-supervisor.service
  Wants=outbe-ocomp-worker.socket

outbe-validator.service
  no Requires=/BindsTo=/PartOf= relationship to any OCOMP unit

outbe-ocomp-worker.socket
  triggers outbe-ocomp-worker@.service per accepted connection
```

`After=outbe-validator.service` may order compute-service startup, but it does
not couple shutdown or failure. Supervisor and exporter reconnect with bounded
backoff. Stopping or failing either cannot stop or restart the node.

This follows the documented distinction: `Wants=` is a weak startup
relationship, while `Requires=`, `BindsTo=` and `PartOf=` propagate stronger
failure, stop or restart behavior. See
[`systemd.unit(5)`](https://man7.org/linux/man-pages/man5/systemd.unit.5.html).

Long-running sibling services use `Restart=on-failure` with a finite
`StartLimitBurst` and observable backoff. Worker instances never restart
themselves: the supervisor retries the same `UnitId` through a new connection.

### 3.3 Fixed worker template without a launch broker

`outbe-ocomp-worker.socket` is a filesystem AF_UNIX stream socket with:

```ini
Accept=yes
MaxConnections=4
MaxConnectionsPerSource=4
SocketUser=outbe-ocomp-supervisor
SocketGroup=outbe-ocomp-worker-launch
SocketMode=0660
```

`Accept=yes` causes one service-template instance per connection, and
`MaxConnections=4` refuses excess concurrent instances. This is the exact PoC
meaning of `workers per validator domain = 1..4`; it does not affect signer
weight. systemd explicitly supports per-connection isolation and a connection
cap for this mode. See
[`systemd.socket(5)`](https://man7.org/linux/man-pages/man5/systemd.socket.5.html).

The supervisor:

1. selects an already frozen `UnitSpecV1` from the complete parent-job plan;
2. opens one connection to the fixed socket;
3. completes the worker handshake;
4. sends exactly one bounded `RunUnitV1`;
5. waits for one result reference or a bounded failure;
6. closes the connection.

The worker:

1. verifies the connecting supervisor UID with `SO_PEERCRED`;
2. binds its invocation to the first valid `UnitId`;
3. verifies bundle, `JobId`, attempt, `PlanHash`, plan membership and all CAS
   object references;
4. executes exactly that unit;
5. writes exactly one staged `UnitArtifactV1`;
6. returns its digest/length/status and exits.

The queue contains only a bounded window of ready unit ordinals. Canonical
`UnitSpecV1` values are derived lazily from `PlanCommitmentV1`; the supervisor
never materializes every unit. Completing a full shard never terminates
discovery: the next authenticated Tribute starts the next shard and its ordinal
is reached normally. A supervisor may use successive worker invocations for
arbitrarily many units, but each invocation receives exactly one immutable
unit.

The primary catalog is streamed from the manifest-committed Tribute chunk
references, not from a second full Tribute vector. Each primary `UnitSpecV1`
binds the exact chunk semantic digest/count/byte limit; its half-open ID range is
the current chunk's first key through the next chunk's first key.

A second run request, different `UnitId`, arbitrary path, executable, shell
command or resource override is rejected. Connection loss cancels the result;
`RuntimeMaxSec` is the final bound if computation does not observe cancellation.

The worker template is fixed in deployment. No polkit rule, D-Bus
`StartTransientUnit`, dynamic command line, privileged helper or launch broker
is part of the PoC.

## 4. Local control planes

There are two distinct local APIs:

1. node `OcompControlV1`, used only by supervisor/exporter;
2. `WorkerControlV1`, used only between supervisor and one worker invocation.

Neither carries bulk objects. Semantic objects carried inside either API are
complete OCB1 bytes and are decoded with the frozen production codec.

### 4.1 Common bounded frame

Both APIs use an AF_UNIX stream with this local operational frame:

```text
u32_be frame_len                 bytes following this field
4 bytes magic                    "OCL1" or "OWR1"
u16_be control_version           exactly 1
u16_be message_kind
u64_be session_generation        0 only during Hello/HelloAck
u64_be request_id                0 for Hello; strictly increasing thereafter
u32_be body_len
body_len bytes canonical body
```

`frame_len` must equal `28 + body_len`. The receiver reads only the first four
bytes, rejects a value above its advertised cap, and only then allocates.
Unknown magic/version/kind, zero or replayed request IDs, stale generation,
truncation, extra bytes and a cap+1 body close the session. No resynchronization
is attempted after a malformed frame.

This frame is local operational transport, not a fork/consensus object. It may
be versioned independently, but it cannot reinterpret OCB1 bytes or accept a
job without one exact common `ProtocolBundleHash`.

The generated deployment limits manifest fixes:

- maximum frame bytes per message kind;
- maximum vector/count value per message kind;
- maximum two concurrent node-control sessions: one supervisor and one
  exporter;
- one outstanding request per session in PoC;
- request and idle timeouts.

The generator derives message caps from maximum production encodings plus the
fixed frame. A zero, unbounded or environment-only limit fails the deployment
gate.

### 4.2 Node method registry and capability ACL

The local method registry is:

| Kind | Message | Allowed peer |
|---:|---|---|
| `0x0001` | `HelloV1` | supervisor, exporter |
| `0x0002` | `HelloAckV1` | node |
| `0x0010` | `ListFinalizedJobsV1` | supervisor |
| `0x0011` | `GetJobSpecV1` | supervisor |
| `0x0012` | `OpenSnapshotLeaseV1` | supervisor |
| `0x0013` | `RenewSnapshotLeaseV1` | exporter |
| `0x0014` | `ListSnapshotHandoffsV1` | exporter |
| `0x0015` | `GetSnapshotHandoffV1` | exporter |
| `0x0016` | `BuildFinalizedIntentProofV1` | exporter |
| `0x0017` | `BuildLysisOpeningsV1` | exporter |
| `0x0018` | `CommitSnapshotExportV1` | exporter |
| `0x0019` | `RequestAttestationV1` | supervisor |
| `0x001a` | `GetOcompHealthV1` | supervisor, exporter |
| `0x7ffe` | `ResponseV1` | node |
| `0x7fff` | `ErrorV1` | either direction |

The `Build*` methods are the typed proof source selected in decision ticket #5.
`CommitSnapshotExportV1` implements its compare-and-set `record_exported`
transition; it accepts only the current `JobId`, lease generation, manifest
hash, exact byte/count totals and export certificate hash.

`HelloV1` and `HelloAckV1` carry the fields already required by the PoC:
chain/genesis, boot/session nonce, supported control versions, exact supported
bundle hashes, capability bits, receive limits, peer identity and selected
session generation. Capability bits are intersected with the fixed
peer-UID role; a caller cannot self-assign capabilities.

Every job method includes the session generation and exact bundle/`JobId`.
`RequestAttestationV1` includes constant-size `LysisResultV1`; it never accepts
a digest-only or generic signing request. Bulk result chunks remain outside the
node control plane.

`ErrorV1` contains only:

```text
rejected_kind: u16
error_code: u16
retryable: bool
```

The closed error set is `MALFORMED`, `LIMIT_EXCEEDED`, `UNAUTHORIZED`,
`STALE_SESSION`, `NO_COMMON_BUNDLE`, `NOT_FOUND`, `CONFLICT`, `BUSY`,
`SOURCE_UNAVAILABLE` and `INTERNAL_OCOMP_UNAVAILABLE`. Error text remains local
structured logging and cannot create an unbounded response.

### 4.3 Worker method registry

The worker API has only:

| Kind | Message |
|---:|---|
| `0x0001` | `WorkerHelloV1` |
| `0x0002` | `WorkerHelloAckV1` |
| `0x0010` | `RunUnitV1` |
| `0x7ffe` | `UnitFinishedV1` |
| `0x7fff` | `WorkerErrorV1` |

`RunUnitV1` contains no path and no executable:

```text
protocol_bundle_hash
JobId
attempt
PlanHash
unit_index
canonical UnitSpecV1 OCB1 bytes
unit_membership_siblings (bottom-up B256 list, at most 32)
canonical PlanCommitmentV1 bytes
transport reference to InputManifestV1
strictly ordered input object transport references
```

A primary unit is accepted only when the canonical `UnitSpecV1` leaf, exact
`unit_index`, `primary_work_unit_count` and bottom-up sibling list reconstruct
`PlanCommitmentV1.primary_work_unit_root`. A one-unit plan has an empty sibling
list; even the maximum `u32` population needs at most 32 siblings. Secondary
units use deterministic derivation from their exact producer UnitIds instead
of this primary-catalog witness.

A transport reference is local-only:

```text
CasObjectRefV1 {
  transport_digest: B256,
  encoded_bytes: u64,
  expected_ocb1_kind: Option<u16>
}
```

The worker recomputes constant-size `PlanHash`, derives the selected
`UnitSpecV1` from its ordinal (or verifies its catalog membership), then
recomputes `UnitId` and validates input semantic roots/counts. It never reads a
complete plan file and cannot use a plan or input supplied only in the request
as authority.

`UnitFinishedV1` returns `UnitId`, status, exact staged byte count and transport
digest. A success does not make the artifact trusted: the supervisor opens the
staged file, verifies it from the same descriptor, adopts it into CAS and then
runs the normal reducer/verifier.

## 5. CAS and scratch layout

### 5.1 Two independent storage budgets

The node OCOMP key/pin/sign-once state and compute artifacts do not share an
unbounded filesystem budget with the chain database:

```text
/var/lib/outbe/ocomp-v1/              node-owned OCOMP project quota
  retention-journal/
  sign-once/
  key/

/var/lib/outbe-ocomp/                 separate compute mount/project quota
  cas-v1/
  exporter-v1/
  supervisor-v1/
  worker-inbox-v1/
```

Filling either budget disables only local export/computation/signing. The chain
database retains reserved free space and continues consensus/execution.

The PoC deployment generator emits both exact quota values. It computes the
compute reservation from one admitted job's maximum source, plan,
intermediate, result, inbox and scratch bytes plus already retained terminal
objects. Admission reserves the complete bound before opening a snapshot lease.
If the current quota cannot cover it, that validator abstains. Local quotas may
be lower than consensus maxima but can never make a larger object eligible.

### 5.2 Exact object paths

For `TransportDigestV1 = keccak256(exact stored bytes)`, the only object path is:

```text
cas-v1/objects/<lowercase first two digest hex>/<lowercase remaining 62 hex>
```

There is no extension, alternate case, semantic-name alias or caller-provided
path. Local administrative files are outside `objects/`:

```text
cas-v1/staging/exporter/
cas-v1/staging/supervisor/
cas-v1/refs/<JobId lowercase hex>/<attempt decimal>/
cas-v1/quarantine/
exporter-v1/journal/
supervisor-v1/journal/
worker-inbox-v1/<UnitId lowercase hex>.ocb1
```

`refs/` records reachability/retention only. It is never input authority.
Deleting or mutating it can cause local loss/abstention but cannot forge a
root, result or signature.

### 5.3 Publish and read rules

CAS publish is:

1. create a same-filesystem role staging file with `O_CREAT|O_EXCL`;
2. stream bytes once while counting and computing `TransportDigestV1`;
3. reject a generated byte/count cap before further growth;
4. `fsync` the staging file;
5. atomically install it at the digest path with no replacement;
6. `fsync` the parent directory;
7. if the object already exists, open that object and verify its exact length
   and digest instead of overwriting it.

CAS read is:

1. derive the path solely from an already bounded digest;
2. open read-only without following symlinks;
3. `fstat` and reject a non-regular file or wrong length;
4. hash and decode from that same descriptor;
5. check end-of-file and an unchanged final `fstat`;
6. verify semantic digest/root/count before use.

Filesystem mode is defense in depth, not the guarantee that data stayed
unchanged. A compromised writer can damage its validator's CAS; exact bytes,
transport digest and authenticated semantic roots detect that damage, so the
domain abstains instead of signing.

Exporter and supervisor may atomically publish verified CAS objects. Worker
processes see `objects/` read-only and write only one derived
`worker-inbox-v1/<UnitId>.ocb1` staging file. The supervisor verifies and adopts
that file; a worker never directly installs an authoritative object.

PoC uses `compression=NONE`; there is no alternate compressed object.

### 5.4 Retention and cleanup

The one-job PoC adapter uses a deterministic local mark/sweep:

1. input, plan, unit and result object refs stay live while the job is
   nonterminal;
2. terminal finality records `release_after_finalized_height`;
3. source/evidence refs remain live for the frozen 64-finalized-block window;
4. after that height, a bounded sweep removes unreferenced objects;
5. a failure or crash during cleanup is retried and cannot make an object live
   again;
6. cleanup has its own I/O budget and is never performed by the node.

Crash-safe multi-job GC, remote CAS and custody repair are BoundedMVP work.
Quota exhaustion before safe release causes local abstention; it never permits
early deletion or changes chain state.

## 6. Resource and filesystem confinement

Every OCOMP service is placed below `outbe-ocomp.slice`; the node is outside
that slice. Checked-in deployment literals are generated from the exact
`OcompPocDevnetMachineV1` profile and 20%-headroom rule frozen in the
[protocol decision](off-chain-poc-protocol-freeze.md#82-minimum-poc-machine-and-headroom-rule),
plus the maximum-shaped corpus:

| Boundary | Required hard controls |
|---|---|
| OCOMP slice | aggregate `CPUQuota`, `MemoryHigh`, `MemoryMax`, `TasksMax`, I/O read/write limits |
| exporter | fixed memory/tasks/runtime/I/O caps; CE bind read-only; only exporter journal and CAS staging writable |
| supervisor | fixed memory/tasks/runtime/I/O caps; no CE/Mongo/key paths; CAS/ref/journal writable |
| each worker | per-instance memory/tasks/runtime/I/O caps; CAS read-only; inbox/scratch only writable |
| relay | independent process budget outside validator OCOMP authority |

`MemoryHigh` is the normal pressure control and `MemoryMax` the final OOM
boundary; CPU, task and per-device I/O controllers cap aggregate use. See
[`systemd.resource-control(5)`](https://man7.org/linux/man-pages/man5/systemd.resource-control.5.html).

The generator must prove, on the minimum machine:

- the node's reserved CPU/memory/I/O floor is met before assigning OCOMP
  resources;
- four maximum worker instances plus exporter/supervisor stay below the OCOMP
  slice ceiling;
- cap succeeds with positive measured headroom;
- cap+1 is refused at admission/socket/parser/quota boundaries rather than by
  host-wide OOM or disk exhaustion.

No runtime request may raise a service-manager limit.

The baseline service sandbox is:

```text
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
PrivateDevices=yes
CapabilityBoundingSet=
RestrictSUIDSGID=yes
LockPersonality=yes
```

Worker additionally uses `PrivateNetwork=yes`, `RestrictAddressFamilies=AF_UNIX`
and a read-only CAS bind. `PrivateNetwork=yes` gives the process only a private
loopback namespace, and `BindReadOnlyPaths=` creates a unit-local read-only
mount. See
[`systemd.exec(5)`](https://man7.org/linux/man-pages/man5/systemd.exec.5.html).

Supervisor has only AF_UNIX plus outbound access to the configured relay.
Exporter has only AF_UNIX plus the fixed read-only Mongo endpoint required by
the PoC. A broad node RPC URL, shell callback, code download or arbitrary
network destination is not a configuration field.

Landlock, seccomp allowlists, user namespaces, remote mTLS and an audited
aggregate launch broker remain BoundedMVP hardening. The PoC must not claim
those properties.

## 7. Failure semantics

| Failure | Required local outcome | Consensus outcome |
|---|---|---|
| supervisor absent/crashed/incompatible | no worker requests or attestation; restart cursor from journal | blocks and finality continue; `ocomp_ready=false` |
| exporter absent/crashed | lease expires or export remains incomplete; validator abstains | unchanged |
| worker crash/timeout/mutation | reject staged bytes; retry same `UnitId` within deadline | unchanged |
| fifth concurrent worker request | socket/service manager refuses it | unchanged |
| unauthorized UDS peer | reject before method/body work | unchanged |
| malformed or cap+1 frame | reject before body allocation/crypto | unchanged |
| CAS object changed | transport/semantic verification fails; quarantine/abstain | unchanged |
| compute CAS/inbox quota full | admission or write fails locally | unchanged |
| node OCOMP journal quota full | no new OCOMP signature/pin acknowledgement | normal consensus except the local vote rule already selected for a required pin |
| no common bundle | close only OCOMP session | node remains consensus-ready |
| relay absent | announcements may wait or another client may submit | unchanged |

No failure invokes synchronous/on-chain Lysis and no compute service may
request node shutdown.

## 8. Harness and production-like test entrypoints

The existing Rust E2E harness remains the sole owner of test subprocesses. It
is extended with typed guards for:

- four node processes;
- four exporter processes;
- four supervisor processes;
- the external relay;
- worker socket activators and every spawned worker;
- four independent CAS volumes/directories and their fault controls.

The supervisor always connects to the real worker socket and sends the real
`WorkerControlV1`; the harness cannot inject a unit executor or result. On an
unprivileged developer lane, a small harness-owned socket activator starts the
exact production `outbe-ocomp worker` entrypoint with an inherited connected
socket. It may emulate process activation only; it may not emulate worker
logic, CAS, result bytes or limits.

That lane proves process death/restart and protocol behavior but cannot claim
UID/cgroup/mount isolation. PoC release closure therefore also requires a
Linux systemd/cgroup-v2 lane using the checked-in units. Lack of that runner is
a reported unmet prerequisite, never a skip-as-success.

`ScenarioEvidence` must be extended later to record:

- executable hashes and exact role arguments;
- PID, real UID/GID, cgroup path, mount namespace and network namespace;
- socket inode/path/mode and observed peer credentials;
- CAS mount/device/quota and role-visible read/write paths;
- systemd `Wants/Requires/BindsTo/PartOf/After` properties;
- start/stop/crash timestamps and node finality heights before/after;
- frame, object and quota cap boundary outcomes.

## 9. Blocking acceptance for this decision

The implementation plan may schedule ticket #7 only against these exact
boundaries. Ticket #6 implementation is complete only when planned tests prove:

1. one package/executable supplies all compute roles while OS permissions
   enforce the role matrix;
2. node starts and finalizes with every OCOMP unit stopped;
3. stopping/killing supervisor, exporter, worker and relay cannot stop a node;
4. four worker connections create four distinct processes/cgroups and a fifth
   is refused;
5. every worker invocation accepts one exact admitted `UnitId` and exits;
6. worker cannot read node/key/CE/Mongo paths, write CAS objects or use the
   default network;
7. exporter cannot attest and supervisor cannot call exporter-only proof
   methods;
8. wrong UID, stale session, replayed counter, wrong bundle and cap+1 frame are
   rejected before privileged work;
9. CAS same-bytes publish is idempotent, different/corrupt bytes reject, and
   readers verify from one descriptor;
10. filling compute storage leaves node storage and finality usable;
11. checked-in unit dependency inspection finds no node lifecycle dependency
    on OCOMP;
12. retained evidence proves the actual PIDs, UIDs, cgroups, mounts, sockets,
    quotas and continuing finalized heights.

Planned command surfaces, to be made real by later tasks, are:

```text
cargo test -p outbe-ocomp-protocol --test local_control_vectors
cargo test -p outbe-ocomp --test cas_atomicity
cargo test -p outbe-ocomp --test worker_one_unit
systemd-analyze verify deploy/systemd/outbe-ocomp-*.{service,socket,target,slice}
cargo run -p outbe-e2e-harness --bin outbe-e2e -- --tags @ocomp-process-boundary
cargo run -p outbe-e2e-harness --bin outbe-e2e -- --tags @ocomp-systemd-isolation
```

These commands are specifications for implementation planning, not claims that
the tests or units already exist.

## 10. PoC-to-BoundedMVP seam

BoundedMVP may replace only local adapters:

| PoC adapter | BoundedMVP evolution |
|---|---|
| systemd per-connection fixed worker template | audited launch broker with aggregate lease accounting |
| local UDS transport | same logical API over hardened UDS or authenticated mTLS |
| filesystem CAS | hardened/recoverable local or remote content store |
| static UIDs/basic sandbox | stronger namespace/Landlock/seccomp/custody policy |
| one-job mark/sweep | crash-safe multi-job GC and capacity reservation |
| trivial HTTP relay | redundant production relayers |

`JobId`, `UnitId`, `PlanHash`, OCB1 semantic bytes, input authority,
deterministic execution, one signer per validator domain, `ResultDigest`,
certificate and certified activation do not change.

## 11. Why no grilling was needed

The only apparent product choice—how to launch isolated workers without a
production broker—is resolved by the source constraints and the platform:
systemd per-connection socket activation is a fixed service-manager template,
caps concurrency and creates one process per immutable request. The remaining
values are measured/generated deployment limits, not product semantics.

No ubiquitous-language term changed, and this decision introduces no new
consensus or generic-framework concept.
