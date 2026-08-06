use std::{fs, path::Path, time::Instant};

use medusa_intelligence::discover_typescript_workspace;

const ITERATIONS: usize = 20;
const FIXTURE_SIZES: [usize; 3] = [100, 1_000, 5_000];

fn main() {
    for source_count in FIXTURE_SIZES {
        benchmark_workspace(source_count);
    }
}

fn benchmark_workspace(source_count: usize) {
    let repository = tempfile::tempdir().expect("benchmark repository");
    write(&repository.path().join("package.json"), "{}\n");
    write(&repository.path().join("tsconfig.json"), "{}\n");
    for index in 0..source_count {
        write(
            &repository.path().join(format!("src/module-{index:05}.ts")),
            &format!("export const value{index} = {index};\n"),
        );
    }
    write(
        &repository.path().join("generated/client.ts"),
        "export const ignored = true;\n",
    );
    write(
        &repository.path().join("node_modules/pkg/index.ts"),
        "export const ignored = true;\n",
    );

    let start = Instant::now();
    let mut expected_fingerprint = None;
    for _ in 0..ITERATIONS {
        let workspace = discover_typescript_workspace(repository.path(), repository.path())
            .expect("workspace discovery");
        assert_eq!(workspace.source_count, source_count);
        match &expected_fingerprint {
            Some(expected) => assert_eq!(&workspace.workspace_fingerprint, expected),
            None => expected_fingerprint = Some(workspace.workspace_fingerprint),
        }
    }
    let elapsed = start.elapsed();
    println!(
        "{{\"adapter\":\"typescript_javascript\",\"sources\":{source_count},\"iterations\":{ITERATIONS},\"elapsed_ms\":{},\"average_us\":{}}}",
        elapsed.as_millis(),
        elapsed.as_micros() / ITERATIONS as u128
    );
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create benchmark directory");
    }
    fs::write(path, content).expect("write benchmark fixture");
}
