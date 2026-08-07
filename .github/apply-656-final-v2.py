from pathlib import Path

registry = Path("crates/medusa-capabilities/src/registry.rs")
text = registry.read_text(encoding="utf-8")

old = "Rust tree-sitter and Python lexical indexing are available; exact per-language levels are reported by semantic_capabilities"
new = "Rust/Python indexing and repository-scoped TypeScript/JavaScript semantic and guarded-refactoring tools are available; exact per-language levels are reported by semantic_capabilities"
if old not in text:
    raise SystemExit("registry capability description not found")
text = text.replace(old, new, 1)

old = "Guardedly rename one unambiguous Rust identifier across indexed definitions and references; fail closed on parse errors, cross-language matches, or stale bytes."
new = "Guardedly rename one unambiguous Rust or TypeScript/JavaScript symbol; fail closed on parse/protocol errors, ambiguity, repository scope drift, incomplete references, or stale bytes."
if old not in text:
    raise SystemExit("symbol_rename description not found")
text = text.replace(old, new, 1)

rename_start = text.index('name: "symbol_rename"')
shell_start = text.index('name: "shell_run"', rename_start)
segment = text[rename_start:shell_start]
old = "RegistryPermission::RepositoryMutation,\n            ],"
new = "RegistryPermission::RepositoryMutation,\n                RegistryPermission::ProcessSpawn,\n            ],"
if old not in segment:
    raise SystemExit("symbol_rename permission block not found")
segment = segment.replace(old, new, 1)
text = text[:rename_start] + segment + text[shell_start:]

old = '''        assert_eq!(
            registry
                .entry("tool.symbol_rename")
                .expect("rename")
                .capability,
            Capability::CodeIntelligence
        );'''
new = '''        let rename = registry.entry("tool.symbol_rename").expect("rename");
        assert_eq!(rename.capability, Capability::CodeIntelligence);
        assert!(
            rename
                .permissions
                .contains(&RegistryPermission::RepositoryMutation)
        );
        assert!(
            rename
                .permissions
                .contains(&RegistryPermission::ProcessSpawn)
        );'''
if old not in text:
    raise SystemExit("registry symbol_rename test assertion not found")
registry.write_text(text.replace(old, new, 1), encoding="utf-8")

workspace = Path("crates/medusa-intelligence/src/typescript_workspace.rs")
text = workspace.read_text(encoding="utf-8")
old = "path::{Component, Path, PathBuf}"
if old not in text:
    raise SystemExit("Component import not found")
text = text.replace(old, "path::{Path, PathBuf}", 1)

old = '''    matches!(
        name.as_ref(),
        "node_modules"
            | ".git"
            | ".medusa"
            | "target"
            | "dist"
            | "build"
            | "coverage"
            | ".next"
            | ".turbo"
            | "out"
    ) || entry.path().components().any(|component| {
        matches!(
            component,
            Component::Normal(value) if value == "generated" || value == "vendor"
        )
    })'''
new = '''    matches!(
        name.as_ref(),
        "node_modules"
            | ".git"
            | ".medusa"
            | "target"
            | "dist"
            | "build"
            | "coverage"
            | ".next"
            | ".turbo"
            | "out"
            | "generated"
            | "vendor"
    )'''
if old not in text:
    raise SystemExit("workspace ignore block not found")
text = text.replace(old, new, 1)

marker = '''    #[test]
    fn root_fallback_is_deterministic_without_config_or_package() {'''
regression = '''    #[test]
    fn repository_parent_named_vendor_does_not_hide_sources() {
        let sandbox = tempfile::tempdir().expect("sandbox");
        let repository = sandbox.path().join("vendor/repository");
        write(&repository.join("package.json"), "{}");
        write(&repository.join("src/main.ts"), "export {};\\n");

        let workspace = discover_typescript_workspace(&repository, &repository)
            .expect("workspace under vendor parent");
        assert_eq!(workspace.source_count, 1);
    }

'''
if marker not in text:
    raise SystemExit("workspace regression insertion point not found")
workspace.write_text(text.replace(marker, regression + marker, 1), encoding="utf-8")
