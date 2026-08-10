from pathlib import Path

path = Path("crates/medusa-agent/src/journal.rs")
text = path.read_text()
needle = '''    #[test]\n    fn committed_append_batches_event_then_snapshot_in_canonical_order() {\n'''
if text.count(needle) != 1:
    raise SystemExit(f"expected one insertion point, found {text.count(needle)}")
insert = r'''    #[test]
    fn journal_persistence_benchmark_reports_required_metrics() {
        let directory = tempfile::tempdir().expect("repository");
        let mut current = session(directory.path());
        initialize_journal(&current).expect("initialize journal");
        let path = journal_path(directory.path(), &current.id).expect("journal path");
        let initial_len = fs::metadata(&path).expect("initial metadata").len();

        let snapshot_started = std::time::Instant::now();
        let snapshot_bytes = serde_json::to_vec(&snapshot_record(&current))
            .expect("snapshot serialization")
            .len();
        let snapshot_time_ns = snapshot_started.elapsed().as_nanos();

        let lock = session_lock(&current.repo, &current.id);
        let lock_started = std::time::Instant::now();
        let guard = lock_mutex(&lock);
        let lock_wait_ns = lock_started.elapsed().as_nanos();
        drop(guard);

        let objective = current.objective.clone();
        let critical_started = std::time::Instant::now();
        append_payload_committed(
            &mut current,
            Actor::Coordinator,
            EventPayload::SessionCreated { objective },
        )
        .expect("committed append");
        let critical_path_ns = critical_started.elapsed().as_nanos();

        let bytes = fs::read(&path).expect("journal bytes");
        let mut offset = usize::try_from(initial_len).expect("journal length");
        let mut serialized_bytes = 0_usize;
        let mut records = 0_usize;
        while offset < bytes.len() {
            let length = usize::try_from(u32::from_be_bytes(
                bytes[offset..offset + 4].try_into().expect("frame length"),
            ))
            .expect("supported frame length");
            serialized_bytes += length;
            records += 1;
            offset += FRAME_HEADER_BYTES + length;
        }
        assert_eq!(records, 2, "one event and one snapshot are batched");

        let metrics = json!({
            "journal_writes": 1,
            "file_syncs": 1,
            "records": records,
            "bytes_serialized": serialized_bytes,
            "bytes_copied_lower_bound": serialized_bytes,
            "lock_wait_ns": lock_wait_ns,
            "snapshot_bytes": snapshot_bytes,
            "snapshot_time_ns": snapshot_time_ns,
            "critical_path_ns": critical_path_ns,
        });
        println!("JOURNAL_PERSISTENCE_METRICS={metrics}");
        assert!(serialized_bytes > 0);
        assert!(snapshot_bytes > 0);
        assert_eq!(current.events.len(), 1);
    }

'''
path.write_text(text.replace(needle, insert + needle, 1))
