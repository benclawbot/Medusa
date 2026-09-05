#!/usr/bin/env python3
"""Static release-workflow integrity guard.

The normal Medusa release path is immutable and tag-driven. This guard rejects
one-shot/version-pinned release writers, repository marker triggers, tag-ref
mutation, release deletion/recreation, broad authority in the automatic
publisher, and unsafe release-signing trust-domain composition.
"""

from __future__ import annotations

import argparse
import re
import sys
import tempfile
from pathlib import Path

SEMVER_TAG = r"v\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?"
RELEASE_MARKER_NAME = re.compile(
    r"(?i)(?:release.*(?:trigger|bootstrap|replace)|(?:trigger|bootstrap|replace).*release)"
)
PINNED_RELEASE_ASSIGNMENT = re.compile(
    rf"(?i)(?:RELEASE_TAG\s*=\s*['\"]{SEMVER_TAG}['\"]|refs/tags/{SEMVER_TAG}|gh\s+release\s+(?:create|edit|delete|upload)\s+['\"]?{SEMVER_TAG})"
)
TAG_FORCE_MUTATION = [
    re.compile(r"(?i)\bgit\s+tag\b[^\n]*\s-f(?:\s|$)"),
    re.compile(r"(?i)\bgit\s+tag\s+-f\b"),
    re.compile(r"(?i)\bgit\s+push\b[^\n]*(?:--force|-f)\b[^\n]*(?:refs/tags|--tags)"),
    re.compile(r"(?is)\bgh\s+api\b.{0,300}(?:--method\s+(?:PATCH|POST)|-X\s+(?:PATCH|POST)).{0,300}(?:git/refs|git/refs/tags).{0,300}refs/tags/"),
]
TAG_CREATION_OR_UPDATE = [
    re.compile(r"(?i)\bgit\s+push\b[^\n]*refs/tags/"),
    re.compile(r"(?is)\bgh\s+api\b.{0,300}(?:git/refs|git/refs/tags).{0,300}refs/tags/"),
]
RELEASE_DELETION = [
    re.compile(r"(?i)\bgh\s+release\s+delete\b"),
    re.compile(r"(?is)\bgh\s+api\b.{0,200}(?:--method\s+DELETE|-X\s+DELETE).{0,300}/releases(?:/|\b)"),
]
MARKER_REFERENCE = re.compile(
    r"(?i)\.github/[A-Za-z0-9._/-]*release[A-Za-z0-9._/-]*(?:trigger|bootstrap|replace)|"
    r"\.github/[A-Za-z0-9._/-]*(?:trigger|bootstrap|replace)[A-Za-z0-9._/-]*release"
)
PRIMARY_SIGNING_SECRET = "MEDUSA_RELEASE_PRIMARY_ED25519_PRIVATE_KEY_PEM"
RECOVERY_SIGNING_SECRET = "MEDUSA_RELEASE_RECOVERY_ED25519_PRIVATE_KEY_PEM"
PRIMARY_SIGNING_WORKFLOW = "sign-release-manifest.yml"
PRIMARY_SIGNING_WORKFLOWS = frozenset({PRIMARY_SIGNING_WORKFLOW, "rolling-main-cli.yml"})
RECOVERY_SIGNING_WORKFLOW = "sign-release-manifest-recovery.yml"
PRIMARY_SIGNING_ENVIRONMENT = "release-signing-primary"
RECOVERY_SIGNING_ENVIRONMENT = "release-signing-recovery"


def release_like(path: Path, text: str) -> bool:
    name = path.name.lower()
    return "release" in name or "gh release" in text.lower() or "refs/tags" in text.lower()


def job_blocks(text: str) -> dict[str, str]:
    blocks: dict[str, list[str]] = {}
    current_job: str | None = None
    in_jobs = False

    for raw in text.splitlines():
        if raw == "jobs:":
            in_jobs = True
            current_job = None
            continue
        if not in_jobs:
            continue
        match = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", raw)
        if match:
            current_job = match.group(1)
            blocks[current_job] = [raw]
            continue
        if re.match(r"^\S", raw):
            break
        if current_job is not None:
            blocks[current_job].append(raw)
    return {name: "\n".join(lines) for name, lines in blocks.items()}


def job_order_and_write_jobs(text: str) -> tuple[list[str], list[str]]:
    jobs = list(job_blocks(text))
    write_jobs: list[str] = []
    for name, block in job_blocks(text).items():
        if re.search(r"(?m)^\s{6}contents:\s*write\s*(?:#.*)?$", block):
            write_jobs.append(name)
    return jobs, write_jobs


def signing_violations(path: Path, text: str) -> list[str]:
    violations: list[str] = []
    has_primary = PRIMARY_SIGNING_SECRET in text
    has_recovery = RECOVERY_SIGNING_SECRET in text

    if has_primary and has_recovery:
        violations.append("primary and recovery signing authorities must use separate workflows")
    if has_primary and path.name not in PRIMARY_SIGNING_WORKFLOWS:
        violations.append("primary release signing key appears outside approved primary signing workflows")
    if has_recovery and path.name != RECOVERY_SIGNING_WORKFLOW:
        violations.append("recovery release signing key appears outside the recovery signing workflow")

    for job, block in job_blocks(text).items():
        job_primary = PRIMARY_SIGNING_SECRET in block
        job_recovery = RECOVERY_SIGNING_SECRET in block
        if not (job_primary or job_recovery):
            continue
        if job_primary and job_recovery:
            violations.append(f"secret-bearing job {job} exposes both signing authorities")
        if "actions/checkout@" in block:
            violations.append(f"secret-bearing release signer {job} checks out repository code")
        if re.search(r"(?<![A-Za-z0-9_.-])scripts/", block):
            violations.append(f"secret-bearing release signer {job} executes repository scripts")

        expected_environment = (
            PRIMARY_SIGNING_ENVIRONMENT if job_primary else RECOVERY_SIGNING_ENVIRONMENT
        )
        if not re.search(
            rf"(?m)^\s{{4}}environment:\s*{re.escape(expected_environment)}\s*(?:#.*)?$",
            block,
        ):
            violations.append(
                f"secret-bearing release signer {job} must use environment {expected_environment}"
            )
    return violations


def workflow_violations(path: Path, text: str) -> list[str]:
    violations: list[str] = []

    if MARKER_REFERENCE.search(text):
        violations.append("release workflow references a committed marker trigger")

    if PINNED_RELEASE_ASSIGNMENT.search(text):
        violations.append("release writer pins a concrete version/tag")

    for pattern in TAG_FORCE_MUTATION:
        if pattern.search(text):
            violations.append("workflow can force-update a release tag")
            break

    # The normal writer must consume an already-created immutable tag. Creating
    # or updating tag refs from a release workflow is therefore forbidden too.
    if release_like(path, text):
        for pattern in TAG_CREATION_OR_UPDATE:
            if pattern.search(text):
                violations.append("release workflow can create or update tag refs")
                break

    for pattern in RELEASE_DELETION:
        if pattern.search(text):
            violations.append("workflow can delete/recreate a published release")
            break

    violations.extend(signing_violations(path, text))

    if path.as_posix().endswith(".github/workflows/publish-release.yml"):
        trigger_block = text.split("permissions:", 1)[0]
        if not re.search(r"(?m)^\s{4}tags:\s*$", trigger_block):
            violations.append("publish-release must be triggered by version tags")
        if re.search(r"(?m)^\s{4}(?:branches|paths):\s*$", trigger_block):
            violations.append("publish-release must not have branch/path push triggers")
        if not re.search(r"(?m)^permissions:\s*\n\s{2}contents:\s*read\s*$", text):
            violations.append("publish-release must default to contents: read")

        jobs, write_jobs = job_order_and_write_jobs(text)
        if len(write_jobs) != 1:
            violations.append("publish-release must have exactly one contents: write job")
        elif not jobs or write_jobs[0] != jobs[-1]:
            violations.append("contents: write must be scoped to the final release job")

        required_bindings = (
            "RELEASE_SHA: ${{ github.sha }}",
            'tag_commit=$(git rev-list -n 1 "$RELEASE_TAG")',
            '"$tag_commit" != "$RELEASE_SHA"',
            '--revision "$RELEASE_SHA"',
            'gh release view "$RELEASE_TAG"',
            'gh release create "$RELEASE_TAG"',
            '--verify-tag',
        )
        for binding in required_bindings:
            if binding not in text:
                violations.append(f"publish-release missing immutable binding: {binding}")

    return list(dict.fromkeys(violations))


def marker_violations(root: Path) -> list[str]:
    github = root / ".github"
    if not github.is_dir():
        return []
    violations: list[str] = []
    for path in github.iterdir():
        if path.is_file() and RELEASE_MARKER_NAME.search(path.name):
            violations.append(f"{path.relative_to(root)}: committed release marker is forbidden")
    return violations


def check_root(root: Path) -> list[str]:
    violations = marker_violations(root)
    workflow_dir = root / ".github" / "workflows"
    if workflow_dir.is_dir():
        for path in sorted([*workflow_dir.glob("*.yml"), *workflow_dir.glob("*.yaml")]):
            text = path.read_text(encoding="utf-8")
            for violation in workflow_violations(path, text):
                violations.append(f"{path.relative_to(root)}: {violation}")
    return violations


def assert_rejected(name: str, text: str, expected: str) -> None:
    path = Path(".github/workflows") / name
    found = workflow_violations(path, text)
    if not any(expected in item for item in found):
        raise AssertionError(f"fixture {name!r} was not rejected for {expected!r}: {found}")


def assert_accepted(name: str, text: str) -> None:
    path = Path(".github/workflows") / name
    found = workflow_violations(path, text)
    if found:
        raise AssertionError(f"safe fixture {name!r} was rejected: {found}")


def self_test() -> None:
    safe = """name: Publish Release
on:
  push:
    tags:
      - \"v*\"
permissions:
  contents: read
jobs:
  validate:
    runs-on: ubuntu-latest
    steps:
      - run: |
          RELEASE_SHA: ${{ github.sha }}
          tag_commit=$(git rev-list -n 1 \"$RELEASE_TAG\")
          if [[ \"$tag_commit\" != \"$RELEASE_SHA\" ]]; then exit 2; fi
          echo --revision \"$RELEASE_SHA\"
          gh release view \"$RELEASE_TAG\"
  publish:
    permissions:
      contents: write
    runs-on: ubuntu-latest
    steps:
      - run: gh release create \"$RELEASE_TAG\" --verify-tag
"""
    safe_violations = workflow_violations(Path(".github/workflows/publish-release.yml"), safe)
    if safe_violations:
        raise AssertionError(f"safe release fixture rejected: {safe_violations}")

    assert_rejected(
        "marker-release.yml",
        """name: Release\non:\n  push:\n    branches: [main]\n    paths:\n      - .github/release-trigger-v1.2.3\njobs: {}\n""",
        "marker trigger",
    )
    assert_rejected(
        "pinned-release.yml",
        """name: Release\njobs:\n  publish:\n    steps:\n      - run: 'RELEASE_TAG=\"v1.2.3\"'\n""",
        "pins a concrete version/tag",
    )
    assert_rejected(
        "force-release.yml",
        """name: Release\njobs:\n  publish:\n    steps:\n      - run: git push --force origin refs/tags/v1.2.3\n""",
        "force-update",
    )
    assert_rejected(
        "delete-release.yml",
        """name: Release\njobs:\n  publish:\n    steps:\n      - run: gh release delete \"$RELEASE_TAG\" --yes\n""",
        "delete/recreate",
    )
    assert_rejected(
        "publish-release.yml",
        """name: Publish Release\non:\n  push:\n    tags:\n      - \"v*\"\npermissions:\n  contents: write\njobs:\n  publish:\n    permissions:\n      contents: write\n""",
        "default to contents: read",
    )

    both_keys = f"""name: Unsafe Signer
jobs:
  sign:
    environment: {PRIMARY_SIGNING_ENVIRONMENT}
    env:
      {PRIMARY_SIGNING_SECRET}: ${{{{ secrets.{PRIMARY_SIGNING_SECRET} }}}}
      {RECOVERY_SIGNING_SECRET}: ${{{{ secrets.{RECOVERY_SIGNING_SECRET} }}}}
    steps: []
"""
    assert_rejected(PRIMARY_SIGNING_WORKFLOW, both_keys, "separate workflows")
    assert_rejected(PRIMARY_SIGNING_WORKFLOW, both_keys, "both signing authorities")

    primary_checkout = f"""name: Unsafe Primary Signer
jobs:
  sign-primary:
    environment: {PRIMARY_SIGNING_ENVIRONMENT}
    env:
      {PRIMARY_SIGNING_SECRET}: ${{{{ secrets.{PRIMARY_SIGNING_SECRET} }}}}
    steps:
      - uses: actions/checkout@deadbeef
"""
    assert_rejected(PRIMARY_SIGNING_WORKFLOW, primary_checkout, "checks out repository code")

    recovery_script = f"""name: Unsafe Recovery Signer
jobs:
  sign-recovery:
    environment: {RECOVERY_SIGNING_ENVIRONMENT}
    env:
      {RECOVERY_SIGNING_SECRET}: ${{{{ secrets.{RECOVERY_SIGNING_SECRET} }}}}
    steps:
      - run: python3 scripts/release-evidence.py sign-manifest
"""
    assert_rejected(RECOVERY_SIGNING_WORKFLOW, recovery_script, "executes repository scripts")

    wrong_environment = f"""name: Unsafe Primary Environment
jobs:
  sign-primary:
    environment: release-signing
    env:
      {PRIMARY_SIGNING_SECRET}: ${{{{ secrets.{PRIMARY_SIGNING_SECRET} }}}}
    steps: []
"""
    assert_rejected(PRIMARY_SIGNING_WORKFLOW, wrong_environment, PRIMARY_SIGNING_ENVIRONMENT)

    safe_primary = f"""name: Safe Primary Signer
jobs:
  prepare:
    steps:
      - uses: actions/checkout@deadbeef
      - run: python3 scripts/release-evidence.py manifest
  sign-primary:
    environment: {PRIMARY_SIGNING_ENVIRONMENT}
    env:
      {PRIMARY_SIGNING_SECRET}: ${{{{ secrets.{PRIMARY_SIGNING_SECRET} }}}}
    steps:
      - run: openssl pkeyutl -sign
"""
    assert_accepted(PRIMARY_SIGNING_WORKFLOW, safe_primary)

    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        (root / ".github").mkdir()
        marker = root / ".github" / "release-trigger-v9.9.9"
        marker.write_text("one shot\n", encoding="utf-8")
        found = marker_violations(root)
        if not found:
            raise AssertionError("release marker fixture was not rejected")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=("check", "self-test"))
    parser.add_argument("--root", default=".")
    args = parser.parse_args()

    if args.command == "self-test":
        self_test()
        print("release workflow policy self-test: ok")
        return 0

    violations = check_root(Path(args.root).resolve())
    if violations:
        print("release workflow policy violations:", file=sys.stderr)
        for violation in violations:
            print(f"- {violation}", file=sys.stderr)
        return 2
    print("release workflow policy: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
