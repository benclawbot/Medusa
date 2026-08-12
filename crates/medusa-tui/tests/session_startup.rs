use std::path::PathBuf;

use medusa_tui::TuiOptions;

#[test]
fn fresh_tui_start_has_no_implicit_resume_mode() {
    let options = TuiOptions::for_repo(PathBuf::from("repo"));
    assert!(options.resume_session.is_none());
    assert!(!options.continue_latest);
}

#[test]
fn explicit_resume_and_continue_modes_remain_distinct() {
    let mut resume = TuiOptions::for_repo(PathBuf::from("repo"));
    resume.resume_session = Some("session-1".to_owned());
    assert_eq!(resume.resume_session.as_deref(), Some("session-1"));
    assert!(!resume.continue_latest);

    let mut latest = TuiOptions::for_repo(PathBuf::from("repo"));
    latest.continue_latest = true;
    assert!(latest.resume_session.is_none());
    assert!(latest.continue_latest);
}
