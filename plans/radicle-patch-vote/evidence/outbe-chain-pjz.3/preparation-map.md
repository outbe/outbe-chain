# P2 external native preparation: implementation boundary

This is a working caller map, not P2 acceptance. Source inspected: 91644eb78148e7b81cd8e46478dfb0466d321e27. No D1 shared-admission implementation or user-delegation feature is authorized by this file.

Required behavior: construct native COB objects from action bytes, embeds, pinned parents/resource and explicit metadata; return exact unsigned bytes before any external signing wait. Attachment verifies external BLS against these bytes and stores only objects. Ref promotion is a later policy-gated step, not a side effect of preparation.

| Boundary | Current source / caller | Required change and proof |
| --- | --- | --- |
| Raw COB preparation | radicle-cob/src/backend/git/change.rs store/write_manifest/write_commit; object/collaboration/{create,update} | Reuse manifest/tree writer and Git Commit metadata serializer; explicit author/committer; deterministic sorted/deduplicated tips/related and unique ordered actual parents; validate real object kinds before asking for a signature. Existing Ed store remains unchanged. |
| Exact attachment | same file attach_native/load_native; exported at radicle-cob/src/lib.rs | Preserve exact metadata and external envelope validation; prepare through production helper in existing real P0 threshold-vector test; confirm actual commit OID, no refs, reopen and tamper negatives. |
| Typed action preparation | radicle/src/cob/store.rs Transaction, encoding::encode, Embed URI conversion | Reuse canonical action serializer and action parents; expose preparation without a synchronous Ed signer or sign_refs call. Reject an empty operation. |
| Identity document phase | radicle/src/identity/doc.rs encode/sign/verify_signature; cob/identity.rs Transaction::new_revision | Canonical Doc::encode already provides actual blob bytes/OID. Verify supplied external document envelope against actual repo/blob and independently supplied context; embed it in a native identity action before COB preparation. Do not demand a publication receipt merely to prepare an unpublished signature. |
| Refs phase | radicle/src/storage/refs/sigrefs/write.rs PreparedSignedRefs | Existing subject/attach uses explicit metadata and previous sigrefs; no refs change. Retain as final preparation phase after COB signatures. |
| Publication | storage/git/cob.rs object::Storage::update; storage/git.rs canonical refs; fetch/cache paths | Incomplete P2 work. No current helper is claimed to implement the complete trusted receipt/CAS gate. This map does not select a D1 design. |

Regression scope: raw COB, typed identity/patch, native refs, legacy Ed workflows; then complete affected native suites/vendor all-target Clippy and root consumers. Full two-peer BLS workflow and all admission paths remain mandatory before P2 can be reviewed or closed.
