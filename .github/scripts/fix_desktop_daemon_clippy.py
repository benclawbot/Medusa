#!/usr/bin/env python3
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def replace_once(path: str, old: str, new: str) -> None:
    target = ROOT / path
    content = target.read_text()
    count = content.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one Clippy correction anchor, found {count}")
    target.write_text(content.replace(old, new, 1))


replace_once(
    "crates/medusa-daemon/src/telegram/command.rs",
    "    Forward(FrontendCommandEnvelope),\n",
    "    Forward(Box<FrontendCommandEnvelope>),\n",
)
replace_once(
    "crates/medusa-daemon/src/telegram/command.rs",
    "    Ok(TelegramInboundAction::Forward(envelope))\n",
    "    Ok(TelegramInboundAction::Forward(Box::new(envelope)))\n",
)
replace_once(
    "crates/medusa-daemon/src/telegram/service.rs",
    "                let acknowledgement = self.control.dispatch(envelope)?;\n",
    "                let acknowledgement = self.control.dispatch(*envelope)?;\n",
)
