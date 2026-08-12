# Transaction safety pipeline

Repository mutations proposed by workers must flow through `medusa-agent::transaction`.

The pipeline:

1. validates worker lease epochs and stale read fingerprints;
2. resolves disjoint and identical proposals deterministically;
3. invokes consensus only for competing multi-worker proposals;
4. requires both coordinator and commit-barrier authorization;
5. captures a rollback journal before mutation;
6. applies changes through the existing symlink-aware atomic repository boundary;
7. emits fingerprints for barrier, rollback journal, coordinator decision, consensus, and final repository state.

Single-worker edits never invoke consensus. Conflicting replacements fail closed before repository mutation. Verification failures after commit must use the captured transaction evidence and rollback journal to restore only transaction-owned paths; unrelated tracked and untracked files are outside the journal.
