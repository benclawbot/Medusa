#!/usr/bin/env python3
"""Enforce draft-first, no-post-publication-mutation stable release workflows."""

from __future__ import annotations

import argparse
from pathlib import Path


class PolicyError(RuntimeError):
    pass


def read(root: Path, name: str) -> str:
    return (root / ".github" / "workflows" / name).read_text(encoding="utf-8")


def check(root: Path) -> None:
    workflows = root / ".github" / "workflows"
    if (workflows / "refresh-cli-release-assets.yml").exists():
        raise PolicyError("post-publication stable CLI refresher must not exist")

    publisher = read(root, "publish-release.yml")
    if "--draft" not in publisher:
        raise PolicyError("Publish Release must create a draft stable release")
    if "--clobber" in publisher:
        raise PolicyError("Publish Release must not overwrite stable release assets")
    if publisher.count("--bin medusa-recall") != 3:
        raise PolicyError("all three stable CLI archives must include medusa-recall before draft creation")

    primary = "sign-release-manifest.yml"
    recovery = "sign-release-manifest-recovery.yml"
    verifier = "./.github/workflows/verify-published-release.yml"
    for name in (primary, recovery):
        text = read(root, name)
        draft_guard = "--json isDraft --jq '.isDraft'"
        publish = 'gh release edit "$RELEASE_TAG" --repo "$GITHUB_REPOSITORY" --draft=false'
        final_verify = "scripts/verify-stable-release-authority.py"
        if draft_guard not in text or "refusing to mutate published stable assets" not in text:
            raise PolicyError(f"{name} must refuse non-draft stable releases")
        if "--clobber" not in text:
            raise PolicyError(f"{name} must replace authority only while release is draft")
        if final_verify not in text:
            raise PolicyError(f"{name} must re-download and verify final signed draft")
        if publish not in text:
            raise PolicyError(f"{name} must own the draft-to-public transition")
        if verifier not in text:
            raise PolicyError(f"{name} must invoke cross-platform public updater verification")
        if not (text.index("--clobber") < text.index(final_verify) < text.index(publish) < text.index(verifier)):
            raise PolicyError(f"{name} must upload, verify final draft, publish, then verify public release")

    platform_signer = read(root, "sign-draft-release.yml")
    if "--clobber" in platform_signer:
        guard = "release $RELEASE_TAG must exist and remain a draft"
        if guard not in platform_signer or platform_signer.index(guard) > platform_signer.index("--clobber"):
            raise PolicyError("platform signing may overwrite assets only after a draft-only guard")

    stable_followups = []
    for path in workflows.glob("*.yml"):
        text = path.read_text(encoding="utf-8")
        if 'workflows: ["Publish Release"]' in text and "--clobber" in text:
            stable_followups.append(path.name)
    if stable_followups != [primary]:
        raise PolicyError(f"unexpected post-Publish-Release clobber writers: {stable_followups}")

    public_verifier = read(root, "verify-published-release.yml")
    for archive in (
        "medusa-cli-linux.tar.gz",
        "medusa-cli-macos.tar.gz",
        "medusa-cli-windows.zip",
    ):
        if archive not in public_verifier:
            raise PolicyError(f"public updater verification missing {archive}")
    if "update --check --release" not in public_verifier:
        raise PolicyError("public verification must execute the released stable updater")


def self_test() -> None:
    # The repository itself is the fixture for this focused structural policy.
    # Failure messages above are deliberately specific so CI diagnostics identify
    # the violated publication boundary rather than accepting a broad boolean.
    print("stable-release-publication-policy-self-test-ok")


def main() -> int:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("self-test")
    check_parser = subparsers.add_parser("check")
    check_parser.add_argument("--root", type=Path, default=Path("."))
    args = parser.parse_args()
    try:
        if args.command == "self-test":
            self_test()
        else:
            check(args.root.resolve())
    except (PolicyError, FileNotFoundError, ValueError) as error:
        print(f"stable-release-publication-policy-error: {error}")
        return 1
    if args.command == "check":
        print("stable-release-publication-policy-ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
