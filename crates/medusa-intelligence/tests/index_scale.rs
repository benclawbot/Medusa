use std::{
    fs,
    mem::size_of,
    path::Path,
    time::{Duration, Instant},
};

use medusa_intelligence::{CodeIndex, IndexSnapshot, Reference, Symbol};

const FILE_COUNT: usize = 1_000;
const FULL_BUILD_CEILING: Duration = Duration::from_secs(60);
const INCREMENTAL_REFRESH_CEILING: Duration = Duration::from_secs(10);
const INDEX_HEAP_CEILING_BYTES: usize = 64 * 1024 * 1024;

#[test]
fn representative_large_repository_stays_within_time_and_memory_ceilings() {
    let repository = tempfile::tempdir().expect("repository");
    write_repository(repository.path(), FILE_COUNT);

    let started = Instant::now();
    let mut index = CodeIndex::build(repository.path()).expect("full index");
    let full_build_elapsed = started.elapsed();

    assert_eq!(index.symbols.len(), FILE_COUNT * 2);
    assert!(
        full_build_elapsed <= FULL_BUILD_CEILING,
        "full build took {full_build_elapsed:?}, ceiling is {FULL_BUILD_CEILING:?}"
    );

    let estimated_heap_bytes = estimated_heap_bytes(&index);
    assert!(
        estimated_heap_bytes <= INDEX_HEAP_CEILING_BYTES,
        "estimated index heap was {estimated_heap_bytes} bytes, ceiling is {INDEX_HEAP_CEILING_BYTES}"
    );

    let before = IndexSnapshot::capture(repository.path()).expect("before snapshot");
    for file_index in 0..10 {
        fs::write(
            repository.path().join(format!("src/module_{file_index:04}.rs")),
            rust_source(file_index, 2),
        )
        .expect("modify source");
    }
    let after = IndexSnapshot::capture(repository.path()).expect("after snapshot");

    let started = Instant::now();
    let refresh = index
        .refresh(repository.path(), &before.diff(&after))
        .expect("incremental refresh");
    let incremental_elapsed = started.elapsed();

    assert_eq!(refresh.reindexed.len(), 10);
    assert!(refresh.removed.is_empty());
    assert!(
        incremental_elapsed <= INCREMENTAL_REFRESH_CEILING,
        "incremental refresh took {incremental_elapsed:?}, ceiling is {INCREMENTAL_REFRESH_CEILING:?}"
    );
    assert_eq!(
        index,
        CodeIndex::build(repository.path()).expect("deterministic rebuild")
    );
}

fn write_repository(root: &Path, file_count: usize) {
    fs::create_dir_all(root.join("src")).expect("src");
    fs::create_dir_all(root.join("target/generated")).expect("generated");
    fs::create_dir_all(root.join("vendor")).expect("vendor");

    for file_index in 0..file_count {
        fs::write(
            root.join(format!("src/module_{file_index:04}.rs")),
            rust_source(file_index, 1),
        )
        .expect("source");
    }

    for file_index in 0..100 {
        fs::write(
            root.join(format!("target/generated/ignored_{file_index:04}.rs")),
            rust_source(file_index, 1),
        )
        .expect("generated source");
        fs::write(
            root.join(format!("vendor/ignored_{file_index:04}.rs")),
            rust_source(file_index, 1),
        )
        .expect("vendor source");
    }
}

fn rust_source(file_index: usize, revision: usize) -> String {
    format!(
        "pub struct Type{file_index:04};\n\npub fn function_{file_index:04}() -> usize {{\n    helper_{file_index:04}() + {revision}\n}}\n\nfn helper_{file_index:04}() -> usize {{ {file_index} }}\n"
    )
}

fn estimated_heap_bytes(index: &CodeIndex) -> usize {
    let symbol_bytes = index.symbols.capacity() * size_of::<Symbol>()
        + index
            .symbols
            .iter()
            .map(|symbol| symbol.name.capacity() + path_capacity(&symbol.path))
            .sum::<usize>();

    let reference_bytes = index.references.capacity() * size_of::<(String, Vec<Reference>)>()
        + index
            .references
            .iter()
            .map(|(name, references)| {
                name.capacity()
                    + references.capacity() * size_of::<Reference>()
                    + references
                        .iter()
                        .map(|reference| {
                            reference.name.capacity() + path_capacity(&reference.path)
                        })
                        .sum::<usize>()
            })
            .sum::<usize>();

    let parse_error_bytes = index.parse_errors.capacity() * size_of::<std::path::PathBuf>()
        + index
            .parse_errors
            .iter()
            .map(|path| path_capacity(path))
            .sum::<usize>();

    symbol_bytes + reference_bytes + parse_error_bytes
}

fn path_capacity(path: &std::path::PathBuf) -> usize {
    path.as_os_str().len()
}
