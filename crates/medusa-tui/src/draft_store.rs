// The durable draft implementation is retained below the wrapper. A normal TUI launch uses the
// reserved `current` key and is a fresh runtime session; restoring that key across process launches
// leaks rejected/unfinished input into the next session. Explicit resume/continue keys remain
// durable and are still restored.

mod base {
    include!("draft_store_base.rs");
}

use std::{io, path::Path};

use crate::clipboard::PromptDraft;

#[derive(Clone, Debug)]
pub struct DraftStore {
    inner: base::DraftStore,
}

impl DraftStore {
    #[must_use]
    pub fn for_repo(repo: &Path) -> Self {
        Self {
            inner: base::DraftStore::for_repo(repo),
        }
    }

    pub fn save(&self, key: &str, draft: &PromptDraft) -> io::Result<()> {
        self.inner.save(key, draft)
    }

    pub fn flush(&self) -> io::Result<()> {
        self.inner.flush()
    }

    pub fn load(&self, key: &str) -> io::Result<Option<PromptDraft>> {
        if key == "current" {
            self.inner.delete(key)?;
            return Ok(None);
        }
        self.inner.load(key)
    }

    pub fn delete(&self, key: &str) -> io::Result<()> {
        self.inner.delete(key)
    }
}

#[cfg(test)]
mod wrapper_tests {
    use super::*;

    #[test]
    fn fresh_current_draft_is_never_restored_across_launches() {
        let repository = tempfile::tempdir().expect("temporary repository");
        let store = DraftStore::for_repo(repository.path());
        store
            .save(
                "current",
                &PromptDraft {
                    text: "he ".to_owned(),
                    ..PromptDraft::default()
                },
            )
            .expect("save fresh-session draft");
        store.flush().expect("flush fresh-session draft");

        let reopened = DraftStore::for_repo(repository.path());
        assert_eq!(reopened.load("current").expect("load current"), None);
        assert_eq!(
            reopened.inner.load("current").expect("inspect persisted current"),
            None,
            "discarded current draft must also be removed from durable storage"
        );
    }

    #[test]
    fn explicit_session_drafts_still_restore() {
        let repository = tempfile::tempdir().expect("temporary repository");
        let store = DraftStore::for_repo(repository.path());
        let draft = PromptDraft {
            text: "resume me".to_owned(),
            ..PromptDraft::default()
        };
        store.save("ses_123", &draft).expect("save resume draft");
        store.flush().expect("flush resume draft");

        let reopened = DraftStore::for_repo(repository.path());
        assert_eq!(
            reopened.load("ses_123").expect("load resume draft"),
            Some(draft)
        );
    }
}
