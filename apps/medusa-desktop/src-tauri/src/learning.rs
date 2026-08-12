use medusa_runtime::learning_review::{
    self, LearningAuditExport, LearningPrivacy, LearningReviewSnapshot, LearningReviewState,
    RedactionPreview,
};

#[tauri::command]
pub fn runtime_learning_review(repo: String) -> Result<LearningReviewSnapshot, String> {
    let repo = canonical_repo(&repo)?;
    learning_review::read(&repo).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn runtime_learning_transition(
    repo: String,
    id: String,
    action: String,
    expected_revision: u64,
) -> Result<LearningReviewSnapshot, String> {
    let repo = canonical_repo(&repo)?;
    let target = match action.as_str() {
        "approve" => LearningReviewState::Approved,
        "reject" => LearningReviewState::Rejected,
        "defer" => LearningReviewState::Deferred,
        "validate" => LearningReviewState::Validated,
        "activate" => LearningReviewState::Active,
        "suspend" => LearningReviewState::Suspended,
        "rollback" => LearningReviewState::RolledBack,
        "delete" => LearningReviewState::Deleted,
        _ => return Err("unsupported learning lifecycle action".to_owned()),
    };
    learning_review::transition(&repo, &id, target, expected_revision, "desktop")
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn runtime_learning_inspect(repo: String, id: String) -> Result<Vec<String>, String> {
    let repo = canonical_repo(&repo)?;
    learning_review::inspect(&repo, &id).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn runtime_learning_propose(
    repo: String,
    scope: String,
    key: String,
    value: String,
) -> Result<LearningReviewSnapshot, String> {
    let repo = canonical_repo(&repo)?;
    learning_review::propose(&repo, &scope, &key, &value).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn runtime_learning_evaluate(
    repo: String,
    id: String,
    passed: bool,
) -> Result<LearningReviewSnapshot, String> {
    let repo = canonical_repo(&repo)?;
    learning_review::evaluate(&repo, &id, passed).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn runtime_learning_privacy(
    repo: String,
    privacy: LearningPrivacy,
    expected_revision: u64,
) -> Result<LearningReviewSnapshot, String> {
    let repo = canonical_repo(&repo)?;
    learning_review::update_privacy(&repo, privacy, expected_revision, "desktop")
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn runtime_learning_redaction_preview(repo: String) -> Result<RedactionPreview, String> {
    let repo = canonical_repo(&repo)?;
    learning_review::redaction_preview(&repo).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn runtime_learning_export(repo: String) -> Result<LearningAuditExport, String> {
    let repo = canonical_repo(&repo)?;
    learning_review::export(&repo).map_err(|error| error.to_string())
}

fn canonical_repo(repo: &str) -> Result<std::path::PathBuf, String> {
    let path = std::path::PathBuf::from(repo);
    if !path.is_dir() {
        return Err("learning review requires an open repository".to_owned());
    }
    path.canonicalize().map_err(|error| error.to_string())
}
