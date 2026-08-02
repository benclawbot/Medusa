#!/usr/bin/env python3
"""Use a cross-thread trust-store handoff in updater integration coverage."""

from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise RuntimeError(f"{label}: expected one match, found {count}")
    return text.replace(old, new, 1)


path = Path("crates/medusa-update/tests/update_manager_coverage.rs")
text = path.read_text(encoding="utf-8")
text = replace_once(
    text,
    '''    path::Path,
    thread,
};''',
    '''    path::Path,
    sync::{Arc, Mutex},
    thread,
};''',
    "shared trust-store imports",
)
text = replace_once(
    text,
    '''    let payload = b"payload".to_vec();
    let (base, worker) = server({
        let payload = payload.clone();
        move |base| {
            let (trust_store, release, manifest, signature) = signed_release(base, &payload);
            TRUST_STORE.with(|slot| *slot.borrow_mut() = Some(trust_store));
            vec![''',
    '''    let payload = b"payload".to_vec();
    let trust_store_slot = Arc::new(Mutex::new(None));
    let (base, worker) = server({
        let payload = payload.clone();
        let trust_store_slot = Arc::clone(&trust_store_slot);
        move |base| {
            let (trust_store, release, manifest, signature) = signed_release(base, &payload);
            *trust_store_slot.lock().expect("trust store lock") = Some(trust_store);
            vec![''',
    "cross-thread trust-store writer",
)
text = replace_once(
    text,
    '''    let trust_store = TRUST_STORE
        .with(|slot| slot.borrow_mut().take())
        .expect("trust store");''',
    '''    let trust_store = trust_store_slot
        .lock()
        .expect("trust store lock")
        .take()
        .expect("trust store");''',
    "cross-thread trust-store reader",
)
text = replace_once(
    text,
    '''thread_local! {
    static TRUST_STORE: std::cell::RefCell<Option<TrustStore>> = const { std::cell::RefCell::new(None) };
}

''',
    "",
    "thread-local trust store removal",
)
path.write_text(text, encoding="utf-8")
