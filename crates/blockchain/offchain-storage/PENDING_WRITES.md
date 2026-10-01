# Pending writes and durable acknowledgement

`PendingOverlayStorage::stage` atomically applies one batch to the logical view
and returns a `PendingWrite` owning the association with those exact mutations.
The generation and the batch are private. There is no numeric acknowledgement
interface or separate generation capture.

`PendingWrite::persist` passes the retained batch to the injected durable
`StorageWriter`. The writer must target the durable storage underlying the
overlay. Successful persistence returns a `PendingDurableReceipt` bound to the
same handle. The receipt can be acknowledged once; it cannot be manufactured
or paired with another pending batch.

Pending writes persist in stage order. The module checks this before calling
the writer. A commit gate covers persistence through receipt acknowledgement
or release, while the logical view can continue accepting newer mutations.
An acknowledged handle cannot be persisted again.

The receipt acknowledges only its own mutations, preserving newer puts,
deletes and partition retirements. A storage error, dropped receipt or dropped
handle does not acknowledge or roll back pending data. The same live handle
can retry its exact batch after an error or a dropped receipt. An abandoned
unacknowledged handle keeps later writes blocked; process restart rebuilds the
projection from its durable checkpoint through the existing node recovery.

Logical-only writes through `StorageWriter` intentionally discard their pending
handle and cannot later be acknowledged. Durable projection uses `stage`.
An empty batch contains no pending mutations and consumes no place in stage
order.

The node retains finality, absolute deadline, error classification and retry
policy. Its production writer checks the deadline after persistence and before
acknowledging the receipt. Late backend success therefore leaves pending intact
and does not advance the durable checkpoint. The projector's injected batch
consumer lets the node obtain the pending handle while applying the logical
checkpoint, without a second generation read.

Public pending tests cover receipt release/retry, ordering before backend I/O,
newer mutations and retirements, and abandoned handles. Node tests exercise the
production writer for ambiguous results and late success. Test orchestration
may queue work, but delegates retry and ACK to the production implementation.
