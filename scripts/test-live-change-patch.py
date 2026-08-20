#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


def load_harness():
    path = Path(__file__).resolve().parent / "live-coding-e2e.py"
    spec = importlib.util.spec_from_file_location("live_coding_e2e_patch_test", path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


HARNESS = load_harness()


def git(repo: Path, *args: str) -> str:
    result = HARNESS.run_checked(["git", *args], cwd=repo, capture=True)
    return (result.stdout or "").strip()


def fixture(root: Path) -> tuple[object, Path, str]:
    repo = root / "fixture"
    (repo / "src").mkdir(parents=True)
    (repo / "src" / "counter.js").write_text("counter=broken\n", encoding="utf-8")
    (repo / "src" / "slugify.py").write_text("slugify='broken'\n", encoding="utf-8")
    (repo / "value.txt").write_text("41\n", encoding="utf-8")
    git(repo, "init", "-q", "-b", "main")
    git(repo, "config", "user.name", "Patch Evidence Test")
    git(repo, "config", "user.email", "patch-evidence@example.invalid")
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "baseline")
    baseline = git(repo, "rev-parse", "HEAD")

    harness = HARNESS.Harness(
        repo_root=root,
        output_dir=root / "artifacts",
        timeout_seconds=1,
        heartbeat_seconds=1,
        api_key="credential-value",
    )
    harness.fixture = repo
    harness.baseline_commit = baseline
    return harness, repo, baseline


def patch_paths(text: str) -> set[str]:
    paths: set[str] = set()
    for line in text.splitlines():
        if not line.startswith("diff --git a/"):
            continue
        left = line.split(" b/", 1)[0]
        paths.add(left.removeprefix("diff --git a/"))
    return paths


class LiveChangePatchTests(unittest.TestCase):
    def test_committed_repair_uses_baseline_to_head_product_diff(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            harness, repo, _baseline = fixture(Path(directory))
            (repo / "src" / "counter.js").write_text("counter=fixed\n", encoding="utf-8")
            (repo / "src" / "slugify.py").write_text("slugify='fixed'\n", encoding="utf-8")
            (repo / "value.txt").write_text("42\n", encoding="utf-8")
            git(repo, "add", "src/counter.js", "src/slugify.py", "value.txt")
            git(repo, "commit", "-q", "-m", "integrated repair")

            (repo / ".medusa" / "sessions").mkdir(parents=True)
            (repo / ".medusa" / "sessions" / "runtime.json").write_text("{}\n", encoding="utf-8")
            (repo / "src" / "__pycache__").mkdir()
            (repo / "src" / "__pycache__" / "slugify.pyc").write_bytes(b"runtime-cache")

            harness.collect()
            patch = (harness.output_dir / "multi-language-repair" / "change.patch").read_text(
                encoding="utf-8"
            )

            self.assertTrue(patch.strip())
            self.assertEqual(
                patch_paths(patch),
                {"src/counter.js", "src/slugify.py", "value.txt"},
            )
            self.assertNotIn(".medusa", patch)
            self.assertNotIn("__pycache__", patch)

    def test_precommit_failure_retains_tracked_worktree_mutation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            harness, repo, _baseline = fixture(Path(directory))
            (repo / "value.txt").write_text("42\n", encoding="utf-8")
            git(repo, "add", "value.txt")
            (repo / ".medusa").mkdir()
            (repo / ".medusa" / "runtime.tmp").write_text("ignore me\n", encoding="utf-8")

            harness.collect()
            patch = (harness.output_dir / "multi-language-repair" / "change.patch").read_text(
                encoding="utf-8"
            )

            self.assertIn("diff --git a/value.txt b/value.txt", patch)
            self.assertIn("+42", patch)
            self.assertNotIn(".medusa", patch)


if __name__ == "__main__":
    unittest.main()
