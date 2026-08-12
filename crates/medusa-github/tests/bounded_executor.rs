use std::path::Path;

use medusa_github::{CommandExecutor, SystemExecutor};

#[test]
fn system_executor_bounds_retained_output_while_the_child_completes() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let arguments = vec![
        "metadata".to_owned(),
        "--format-version".to_owned(),
        "1".to_owned(),
        "--no-deps".to_owned(),
    ];
    let output = SystemExecutor
        .run_bounded("cargo", &arguments, Some(&workspace), 64, 64)
        .expect("bounded cargo metadata");
    assert!(output.success, "{}", output.stderr);
    assert_eq!(output.stdout.len(), 65);
    assert!(output.stderr.len() <= 65);
}
