from pathlib import Path


def extract(source: str, target: str) -> None:
    source_path = Path(source)
    target_path = source_path.parent / target
    text = source_path.read_text()
    marker = "#[cfg(test)]\nmod tests {"
    if marker not in text:
        if target_path.exists():
            return
        raise SystemExit(f"inline test marker missing in {source}")
    prefix, module = text.split(marker, 1)
    module = module.strip()
    if not module.endswith("}"):
        raise SystemExit(f"test module is not final in {source}")
    body = module[:-1].strip() + "\n"
    source_path.write_text(prefix.rstrip() + f'\n\n#[cfg(test)]\n#[path = "{target}"]\nmod tests;\n')
    target_path.write_text(body)


extract("crates/medusa-extensions/src/desktop_commander.rs", "desktop_commander_tests.rs")
extract("crates/medusa-tui/src/render/support.rs", "support_tests.rs")
