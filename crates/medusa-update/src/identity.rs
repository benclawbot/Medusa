use serde::Serialize;

/// The channel selected by an installation.  Stable releases and rolling main
/// builds have different publication and rollback semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateChannel {
    Stable,
    Main,
}

/// The source revision embedded in a build. `None` means the binary was built
/// outside a Git checkout or the build metadata was unavailable.
pub type SourceRevision = Option<String>;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct InstalledIdentity {
    pub release_id: String,
    pub source_revision: SourceRevision,
    pub channel: UpdateChannel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PublicationState {
    Verified,
    Unpublished,
    Offline,
    SignatureFailure,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "state")]
pub enum UpdateAvailability {
    UpToDate { revision: String },
    Available { revision: String },
    PendingPublication { revision: String },
    UnknownInstalledRevision { revision: String },
}

/// Compares immutable source identities without relying on SemVer. A rebuild
/// of the same package version is therefore still distinguishable.
#[must_use]
pub fn compare_source_revision(
    installed: SourceRevision,
    available: &str,
    publication: PublicationState,
) -> UpdateAvailability {
    if installed.as_deref() == Some(available) && publication == PublicationState::Verified {
        return UpdateAvailability::UpToDate {
            revision: available.to_owned(),
        };
    }
    if installed.is_none() {
        return UpdateAvailability::UnknownInstalledRevision {
            revision: available.to_owned(),
        };
    }
    if publication == PublicationState::Verified {
        UpdateAvailability::Available {
            revision: available.to_owned(),
        }
    } else {
        UpdateAvailability::PendingPublication {
            revision: available.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CURRENT: &str = "0123456789abcdef0123456789abcdef01234567";
    const NEXT: &str = "1123456789abcdef0123456789abcdef01234567";

    #[test]
    fn identity_outcomes_distinguish_revision_and_publication_state() {
        assert!(matches!(
            compare_source_revision(
                Some(CURRENT.to_owned()),
                CURRENT,
                PublicationState::Verified
            ),
            UpdateAvailability::UpToDate { .. }
        ));
        assert!(matches!(
            compare_source_revision(Some(CURRENT.to_owned()), NEXT, PublicationState::Verified),
            UpdateAvailability::Available { .. }
        ));
        assert!(matches!(
            compare_source_revision(
                Some(CURRENT.to_owned()),
                NEXT,
                PublicationState::Unpublished
            ),
            UpdateAvailability::PendingPublication { .. }
        ));
        assert!(matches!(
            compare_source_revision(None, NEXT, PublicationState::Verified),
            UpdateAvailability::UnknownInstalledRevision { .. }
        ));
        assert!(matches!(
            compare_source_revision(Some(CURRENT.to_owned()), NEXT, PublicationState::Offline),
            UpdateAvailability::PendingPublication { .. }
        ));
        assert!(matches!(
            compare_source_revision(
                Some(CURRENT.to_owned()),
                NEXT,
                PublicationState::SignatureFailure
            ),
            UpdateAvailability::PendingPublication { .. }
        ));
    }
}
