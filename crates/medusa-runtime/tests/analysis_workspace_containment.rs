use std::fs;

use medusa_config::Config;
use medusa_runtime::{
    RuntimeController,
    analysis_workspace::{AnalysisOperation, AnalysisValue},
};
use tempfile::TempDir;

#[test]
fn contained_reducer_processes_brokered_artifact_without_repository_write_authority() {
    let temp = TempDir::new().expect("tempdir");
    fs::write(temp.path().join("large.txt"), "alpha\nbeta\nalpha two\n").expect("fixture");
    let controller =
        RuntimeController::start_with_config(temp.path().to_path_buf(), Config::default());
    let artifact = controller
        .analysis_import_file("session-contained", "large.txt", None)
        .expect("import");
    let result = controller.analysis_reduce_contained(
        "session-contained",
        &artifact,
        AnalysisOperation::MatchingLines {
            needle: "alpha".to_owned(),
            limit: 8,
        },
    );

    if let Err(_error) = &result {
        #[cfg(windows)]
        {
            let message = _error.to_string();
            assert!(
                message.contains("sandbox unavailable")
                    && message.contains("Windows composable sandbox")
                    && message.contains("Windows 11 support is required"),
                "unsupported Windows hosts must fail closed with the effective sandbox boundary: {message}"
            );
            assert_eq!(
                fs::read_to_string(temp.path().join("large.txt")).expect("source remains readable"),
                "alpha\nbeta\nalpha two\n"
            );
            return;
        }
        #[cfg(target_os = "linux")]
        {
            let message = _error.to_string();
            let namespace_denied = message.contains("contained analysis backend failed")
                && message.contains("bwrap: loopback")
                && message.contains("Operation not permitted");
            let backend_unavailable = message.contains("sandbox unavailable")
                && message.contains("Linux bubblewrap sandbox unavailable");
            assert!(
                namespace_denied || backend_unavailable,
                "restricted Linux hosts must fail closed when bubblewrap is unavailable or the kernel denies the isolated network namespace: {message}"
            );
            assert_eq!(
                fs::read_to_string(temp.path().join("large.txt")).expect("source remains readable"),
                "alpha\nbeta\nalpha two\n"
            );
            return;
        }
    }

    let result = result.expect("contained reduction");
    assert_eq!(
        result.result.value,
        AnalysisValue::StringList(vec!["alpha".to_owned(), "alpha two".to_owned()])
    );
    assert_eq!(result.result.provenance[0].artifact_sha256, artifact.sha256);
    assert_eq!(result.metrics.input_bytes, artifact.size_bytes);
    assert!(result.metrics.output_bytes < artifact.size_bytes as usize + 512);
    assert_eq!(
        fs::read_to_string(temp.path().join("large.txt")).expect("source remains readable"),
        "alpha\nbeta\nalpha two\n"
    );
}

#[test]
fn capability_contract_exposes_fail_closed_authority() {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        RuntimeController::start_with_config(temp.path().to_path_buf(), Config::default());
    let capabilities = controller.analysis_workspace_capabilities();
    assert!(!capabilities.arbitrary_user_code);
    assert!(!capabilities.ambient_network);
    assert!(!capabilities.ambient_credentials);
    assert!(!capabilities.primary_repository_write);
    assert!(!capabilities.direct_provider_client);
    assert_eq!(capabilities.process_limit, 1);
}
