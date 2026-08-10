from pathlib import Path

path = Path("crates/medusa-agent/src/journal.rs")
text = path.read_text()
needle = '''    #[test]\n    fn checksum_corruption_fails_closed() {\n'''
if text.count(needle) != 1:
    raise SystemExit(f"expected one insertion point, found {text.count(needle)}")
insert = r'''    #[test]
    fn committed_append_batches_event_then_snapshot_in_canonical_order() {
        let directory = tempfile::tempdir().expect("repository");
        let mut current = session(directory.path());
        let objective = current.objective.clone();
        append_payload_committed(
            &mut current,
            Actor::Coordinator,
            EventPayload::SessionCreated { objective },
        )
        .expect("committed append");

        let path = journal_path(directory.path(), &current.id).expect("journal path");
        let bytes = fs::read(path).expect("journal bytes");
        let mut offset = JOURNAL_MAGIC.len();
        let mut records = Vec::new();
        while offset < bytes.len() {
            let length = usize::try_from(u32::from_be_bytes(
                bytes[offset..offset + 4].try_into().expect("frame length"),
            ))
            .expect("supported length");
            let payload_start = offset + FRAME_HEADER_BYTES;
            let payload_end = payload_start + length;
            records.push(
                serde_json::from_slice::<JournalRecord>(&bytes[payload_start..payload_end])
                    .expect("journal record"),
            );
            offset = payload_end;
        }

        assert_eq!(records.len(), 3, "initial snapshot plus one commit batch");
        assert!(matches!(records[1], JournalRecord::Event { .. }));
        assert!(matches!(records[2], JournalRecord::Snapshot { cursor: 1, .. }));
        let replay = replay_from_cursor(directory.path(), &current.id, 0).expect("replay");
        assert_eq!(replay, current.events);
        verify_chain(&replay).expect("hash chain");
    }

    #[test]
    fn torn_snapshot_in_commit_batch_discards_uncommitted_event() {
        let directory = tempfile::tempdir().expect("repository");
        let mut current = session(directory.path());
        let objective = current.objective.clone();
        append_payload_committed(
            &mut current,
            Actor::Coordinator,
            EventPayload::SessionCreated { objective },
        )
        .expect("committed append");
        let path = journal_path(directory.path(), &current.id).expect("journal path");
        let bytes = fs::read(&path).expect("journal bytes");

        let mut offset = JOURNAL_MAGIC.len();
        let mut frame_starts = Vec::new();
        while offset < bytes.len() {
            frame_starts.push(offset);
            let length = usize::try_from(u32::from_be_bytes(
                bytes[offset..offset + 4].try_into().expect("frame length"),
            ))
            .expect("supported length");
            offset += FRAME_HEADER_BYTES + length;
        }
        assert_eq!(frame_starts.len(), 3);
        let final_snapshot_start = frame_starts[2];
        fs::write(&path, &bytes[..final_snapshot_start + FRAME_HEADER_BYTES + 1])
            .expect("tear final snapshot frame");

        let empty = session(directory.path());
        let mut compatibility = empty.clone();
        compatibility.id = current.id.clone();
        let outcome = load_or_migrate(directory.path(), &current.id, Some(compatibility))
            .expect("recover prior commit");
        assert!(outcome.session.events.is_empty());
        assert!(
            replay_from_cursor(directory.path(), &current.id, 0)
                .expect("replay")
                .is_empty()
        );
    }

    #[test]
    fn journal_locks_are_shared_per_session_but_isolated_across_sessions() {
        let first_repo = tempfile::tempdir().expect("first repository");
        let second_repo = tempfile::tempdir().expect("second repository");
        let first_id = SessionId::new();
        let second_id = SessionId::new();

        let first = session_lock(first_repo.path(), &first_id);
        let same = session_lock(first_repo.path(), &first_id);
        let other_session = session_lock(first_repo.path(), &second_id);
        let other_repo = session_lock(second_repo.path(), &first_id);

        assert!(Arc::ptr_eq(&first, &same));
        assert!(!Arc::ptr_eq(&first, &other_session));
        assert!(!Arc::ptr_eq(&first, &other_repo));
    }

'''
path.write_text(text.replace(needle, insert + needle, 1))
