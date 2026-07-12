# medusa-memory internals

The crate keeps canonical memory in Markdown and treats SQLite as a disposable, rebuildable index. Public APIs remain re-exported from `lib.rs`; implementation is split across schema, proposal, persistence, index, retrieval, lifecycle, engine, and support modules.
