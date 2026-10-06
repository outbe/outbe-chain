//! Copied native closure and replay completeness.
use super::*;

#[test]
fn copied_native_closure_reads_c_plus_one_across_batches_and_reopens_at_k() {
    for closed_height in [3, 205] {
        let donor = tempfile::tempdir().unwrap();
        let receiver = tempfile::tempdir().unwrap();
        let points = write_frames(&donor.path().join("chain"), 0, 205);
        let domain = donor.path().join("ocomp");
        let bundle = bundle();
        let mut initial = runtime(
            provider(&donor.path().join("chain")),
            &domain,
            bundle.clone(),
        );
        catch_up(&mut initial, points[closed_height]);
        drop(initial);
        copy_tree(donor.path(), receiver.path());
        assert!(
            receiver
                .path()
                .join("chain/db/mdbx.dat")
                .metadata()
                .unwrap()
                .len()
                > 0
        );
        donor.close().unwrap();
        let mut restored = runtime(
            provider(&receiver.path().join("chain")),
            &receiver.path().join("ocomp"),
            bundle.clone(),
        );
        let source = RethFinalizedFrameSource::new(restored.provider.clone());
        let mut visited = Vec::new();
        let mut batches = 0;
        while let Some(batch) = read_bounded_finalized_frames(
            &source,
            restored.scanned_height + 1,
            (205, points[205].block_hash).into(),
        )
        .unwrap()
        {
            batches += 1;
            for frame in batch.frames() {
                visited.push(frame.identity().number);
                restored.record_scanned_frame(frame).unwrap();
            }
            restored.flush_closure_checkpoint().unwrap();
        }
        assert_eq!(
            visited,
            ((closed_height as u64 + 1)..=205).collect::<Vec<_>>()
        );
        assert_eq!(batches, if closed_height == 3 { 3 } else { 0 });
        assert_eq!(restored.closure_checkpoint.current().unwrap(), points[205]);
        drop(source);
        drop(restored);
        let later = write_frames(&receiver.path().join("chain"), 206, 207);
        let mut continued = runtime(
            provider(&receiver.path().join("chain")),
            &receiver.path().join("ocomp"),
            bundle.clone(),
        );
        let source = RethFinalizedFrameSource::new(continued.provider.clone());
        let batch = read_bounded_finalized_frames(
            &source,
            continued.scanned_height + 1,
            (207, later[1].block_hash).into(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(batch.frames()[0].identity().number, 206);
        for frame in batch.frames() {
            continued.record_scanned_frame(frame).unwrap();
        }
        assert_eq!(
            continued.flush_closure_checkpoint().unwrap(),
            Some(later[1])
        );
        drop(source);
        drop(continued);
        let second = runtime(
            provider(&receiver.path().join("chain")),
            &receiver.path().join("ocomp"),
            bundle,
        );
        assert_eq!(second.closure_checkpoint.current().unwrap(), later[1]);
        assert_eq!(second.scanned_height, 207);
    }
}

#[test]
fn copied_missing_replay_body_or_receipt_fails_without_advancing_closure() {
    for missing in ["body", "receipt"] {
        let donor = tempfile::tempdir().unwrap();
        let receiver = tempfile::tempdir().unwrap();
        let points = write_frames(&donor.path().join("chain"), 0, 5);
        let mut initial = runtime(
            provider(&donor.path().join("chain")),
            &donor.path().join("ocomp"),
            bundle(),
        );
        catch_up(&mut initial, points[3]);
        drop(initial);
        copy_tree(donor.path(), receiver.path());
        donor.close().unwrap();
        let db = init_db(receiver.path().join("chain/db"), DatabaseArguments::test()).unwrap();
        let tx = db.tx_mut().unwrap();
        if missing == "receipt" {
            tx.delete::<tables::Receipts<OutbeReceipt>>(3, None)
                .unwrap();
        } else {
            tx.delete::<tables::BlockBodyIndices>(4, None).unwrap();
        }
        tx.commit().unwrap();
        drop(db);
        let restored = runtime(
            provider(&receiver.path().join("chain")),
            &receiver.path().join("ocomp"),
            bundle(),
        );
        let source = RethFinalizedFrameSource::new(restored.provider.clone());
        let error = read_bounded_finalized_frames(&source, 4, (5, points[5].block_hash).into())
            .unwrap_err();
        assert!(
            format!("{error:#}").contains(if missing == "receipt" {
                "receipt"
            } else {
                "block"
            }),
            "{error:#}"
        );
        assert_eq!(restored.closure_checkpoint.current().unwrap(), points[3]);
        assert_eq!(restored.scanned_height, 3);
    }
}
