#!/usr/bin/env python3
"""Prefetch and verify the Linux helper tools used by Tauri's AppImage bundler."""

from __future__ import annotations

import argparse
import hashlib
import os
import shutil
import stat
import time
import urllib.request
from pathlib import Path
from typing import Callable, NamedTuple


DOWNLOAD_TIMEOUT_SECONDS = 60
MAX_ATTEMPTS = 4


class Tool(NamedTuple):
    filename: str
    url: str
    sha256: str


TOOLS = (
    Tool(
        "AppRun-x86_64",
        "https://github.com/tauri-apps/binary-releases/releases/download/apprun-old/AppRun-x86_64",
        "f30140a43a0a59e46db21bdefdf749b9e9f2c6946e92afabbacf98b8ae73fb4f",
    ),
    Tool(
        "linuxdeploy-x86_64.AppImage",
        "https://github.com/tauri-apps/binary-releases/releases/download/linuxdeploy/linuxdeploy-x86_64.AppImage",
        "e762bea85c8eb0d4b3508d46e5c1f037f717d0f9303ae3b4aafc8b04991fa1ef",
    ),
    Tool(
        "linuxdeploy-plugin-gtk.sh",
        "https://raw.githubusercontent.com/tauri-apps/linuxdeploy-plugin-gtk/b5eb8d05b4c0ed40107fe2158c5d8527f94568ef/linuxdeploy-plugin-gtk.sh",
        "cb379f9b0733e9ad9f8bd78f8c2fa038aef2478523bb7d4c8e64ff6a1ea3501a",
    ),
    Tool(
        "linuxdeploy-plugin-gstreamer.sh",
        "https://raw.githubusercontent.com/tauri-apps/linuxdeploy-plugin-gstreamer/2a2e67491c32995a3f279ad0ecbe77abd512b42a/linuxdeploy-plugin-gstreamer.sh",
        "c107b49d84edbffc6ab226ed1007e0626a4f7aa2c3a36b7782bef62351d49e94",
    ),
)


class ToolDownloadError(RuntimeError):
    """Raised when a pinned AppImage helper cannot be downloaded safely."""


def digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def ensure_tool(
    tool: Tool,
    cache_dir: Path,
    *,
    opener: Callable = urllib.request.urlopen,
    sleeper: Callable[[float], None] = time.sleep,
) -> Path:
    cache_dir.mkdir(parents=True, exist_ok=True)
    target = cache_dir / tool.filename
    if target.is_file() and digest(target) == tool.sha256:
        return target
    target.unlink(missing_ok=True)

    partial = cache_dir / f"{tool.filename}.part"
    last_error: Exception | None = None
    for attempt in range(1, MAX_ATTEMPTS + 1):
        partial.unlink(missing_ok=True)
        try:
            with opener(tool.url, timeout=DOWNLOAD_TIMEOUT_SECONDS) as response:
                with partial.open("wb") as destination:
                    shutil.copyfileobj(response, destination)
            actual = digest(partial)
            if actual != tool.sha256:
                raise ToolDownloadError(
                    f"SHA-256 mismatch for {tool.filename}: expected {tool.sha256}, got {actual}"
                )
            partial.replace(target)
            os.chmod(
                target,
                target.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH,
            )
            return target
        except (OSError, ToolDownloadError) as error:
            last_error = error
            partial.unlink(missing_ok=True)
            if attempt < MAX_ATTEMPTS:
                sleeper(2 ** (attempt - 1))

    raise ToolDownloadError(
        f"failed to prepare {tool.filename} after {MAX_ATTEMPTS} attempts: {last_error}"
    )


def default_cache_dir() -> Path:
    base = os.environ.get("XDG_CACHE_HOME")
    return (Path(base) if base else Path.home() / ".cache") / "tauri"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--cache-dir", type=Path, default=default_cache_dir())
    args = parser.parse_args()
    for tool in TOOLS:
        path = ensure_tool(tool, args.cache_dir)
        print(f"verified {path.name} {tool.sha256}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
