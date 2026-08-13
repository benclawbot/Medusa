#!/usr/bin/env python3
"""Regression tests for verified Tauri Linux tool prefetching."""

from __future__ import annotations

import hashlib
import importlib.util
import io
import sys
import tempfile
from pathlib import Path


SCRIPT = Path(__file__).with_name("prepare-tauri-linux-tools.py")
sys.dont_write_bytecode = True


def load_module():
    spec = importlib.util.spec_from_file_location("prepare_tauri_linux_tools", SCRIPT)
    if spec is None or spec.loader is None:
        raise AssertionError(f"cannot load {SCRIPT}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class Response(io.BytesIO):
    def __enter__(self):
        return self

    def __exit__(self, *_args):
        self.close()


def main() -> int:
    module = load_module()
    payload = b"verified tauri tool\n"
    checksum = hashlib.sha256(payload).hexdigest()
    tool = module.Tool("tool", "https://example.invalid/tool", checksum)

    with tempfile.TemporaryDirectory() as directory:
        cache = Path(directory)
        attempts = 0

        def flaky_opener(_url, *, timeout):
            nonlocal attempts
            assert timeout == module.DOWNLOAD_TIMEOUT_SECONDS
            attempts += 1
            if attempts == 1:
                raise OSError("peer disconnected")
            return Response(payload)

        path = module.ensure_tool(tool, cache, opener=flaky_opener, sleeper=lambda _: None)
        assert path.read_bytes() == payload
        assert attempts == 2

        def unexpected_download(_url, *, timeout):
            raise AssertionError(f"verified cache should be reused (timeout={timeout})")

        assert module.ensure_tool(tool, cache, opener=unexpected_download) == path

    with tempfile.TemporaryDirectory() as directory:
        cache = Path(directory)
        bad_tool = module.Tool("bad", "https://example.invalid/bad", "0" * 64)
        try:
            module.ensure_tool(
                bad_tool,
                cache,
                opener=lambda _url, *, timeout: Response(payload),
                sleeper=lambda _: None,
            )
        except module.ToolDownloadError as error:
            assert "SHA-256 mismatch" in str(error)
        else:
            raise AssertionError("checksum mismatch must fail closed")
        assert not (cache / "bad").exists()
        assert not list(cache.glob("*.part"))

    print("prepare-tauri-linux-tools self-test passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
