from pathlib import Path
import re

path = Path('scripts/apply-650-runtime-authority.py')
text = path.read_text()
replacement = '''test_anchor = '    #[test]\\n    fn independent_tasks_run_in_parallel() {'
test = ''' + "'''" + '''    #[test]
    fn scheduler_rejects_invalid_evidence_dependency() {
        use medusa_evidence::{EvidenceBundle, EvidenceDependency};
        let directory = tempfile::tempdir().expect("tempdir");
        let planned = plan_typed(PlannerInput {
            objective: "Fix src/lib.rs".to_owned(),
            attachment_count: 0,
            repository_paths: vec!["src/lib.rs".to_owned()],
        })
        .expect("plan");
        let mut ledger = ExecutionLedger::open_or_create(
            directory.path().join("execution.json"),
            &planned,
        )
        .expect("ledger");
        ledger.begin("analyze", "planner").expect("begin");
        let bundle = EvidenceBundle::new("repo", "commit");
        let invalid = EvidenceDependency {
            bundle_fingerprint: "stale".to_owned(),
            decision_ids: Vec::new(),
            fingerprint: "corrupt".to_owned(),
        };
        assert!(ledger
            .succeed_with_evidence("analyze", "summary", &invalid, &bundle)
            .is_err());
    }

''' + "'''" + '''
'''
pattern = r"test_anchor = '    #\[test\]\\n    fn ledger_recovers_interrupted_running_tasks\(\) \{'\ntest = '''.*?'''\n"
text, count = re.subn(pattern, replacement, text, count=1, flags=re.S)
if count != 1:
    raise SystemExit(f'expected one obsolete scheduler test block, found {count}')
path.write_text(text)
