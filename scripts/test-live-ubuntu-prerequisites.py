#!/usr/bin/env python3
from __future__ import annotations

import os
import pathlib
import stat
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
HELPER = ROOT / "scripts" / "install-live-ubuntu-prerequisites.sh"
WORKFLOWS = [
    ROOT / ".github" / "workflows" / "live-provider-dogfood.yml",
    ROOT / ".github" / "workflows" / "live-minimax-tui.yml",
]


def executable(path: pathlib.Path, content: str) -> None:
    path.write_text(content, encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


def run_helper(bin_dir: pathlib.Path, *, force: bool = False) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}{os.pathsep}{env['PATH']}"
    if force:
        env["MEDUSA_FORCE_LIVE_APT_BOOTSTRAP"] = "1"
    else:
        env.pop("MEDUSA_FORCE_LIVE_APT_BOOTSTRAP", None)
    return subprocess.run(
        ["bash", str(HELPER)],
        cwd=ROOT,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=5,
        check=False,
    )


def test_existing_bwrap_skips_apt() -> None:
    with tempfile.TemporaryDirectory() as temporary:
        bin_dir = pathlib.Path(temporary)
        marker = bin_dir / "sudo-called"
        executable(bin_dir / "bwrap", "#!/usr/bin/env bash\nexit 0\n")
        executable(
            bin_dir / "sudo",
            f"#!/usr/bin/env bash\ntouch {marker!s}\nexit 99\n",
        )
        result = run_helper(bin_dir)
        assert result.returncode == 0, result.stdout
        assert "skipping apt bootstrap" in result.stdout
        assert not marker.exists(), "apt bootstrap ran even though bwrap was already available"


def test_unavailable_package_source_fails_promptly_and_explicitly() -> None:
    with tempfile.TemporaryDirectory() as temporary:
        bin_dir = pathlib.Path(temporary)
        executable(
            bin_dir / "timeout",
            """#!/usr/bin/env bash
set -euo pipefail
while (($#)); do
  case "$1" in
    --signal=*|--kill-after=*) shift ;;
    *s) shift; break ;;
    *) break ;;
  esac
done
exec "$@"
""",
        )
        executable(
            bin_dir / "sudo",
            """#!/usr/bin/env bash
echo "Temporary failure resolving 'unavailable.invalid'" >&2
exit 100
""",
        )
        result = run_helper(bin_dir, force=True)
        assert result.returncode == 2, result.stdout
        assert "Live prerequisite unavailable" in result.stdout
        assert "apt update failed or exceeded 120s" in result.stdout
        assert "Temporary failure resolving 'unavailable.invalid'" in result.stdout


def test_bounded_policy_is_shared_by_both_live_workflows() -> None:
    helper = HELPER.read_text(encoding="utf-8")
    for required in (
        "command -v bwrap",
        "Acquire::Retries=2",
        "Acquire::http::ConnectTimeout=10",
        "Acquire::https::ConnectTimeout=10",
        "timeout --signal=TERM --kill-after=10s 120s",
        "Live prerequisite unavailable",
    ):
        assert required in helper, f"missing bounded bootstrap contract: {required}"

    for workflow in WORKFLOWS:
        text = workflow.read_text(encoding="utf-8")
        assert "bash scripts/install-live-ubuntu-prerequisites.sh" in text, workflow
        assert "sudo apt-get update" not in text, workflow


if __name__ == "__main__":
    test_existing_bwrap_skips_apt()
    test_unavailable_package_source_fails_promptly_and_explicitly()
    test_bounded_policy_is_shared_by_both_live_workflows()
    print("live Ubuntu prerequisite bootstrap contract: ok")
