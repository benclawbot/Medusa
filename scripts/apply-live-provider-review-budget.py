#!/usr/bin/env python3
from pathlib import Path

path = Path('.github/workflows/release-gates.yml')
text = path.read_text(encoding='utf-8')
old = '''  live-minimax-coding-e2e:
    if: ${{ github.event_name == 'workflow_dispatch' || github.event.pull_request.draft == false }}
    runs-on: ubuntu-latest
    timeout-minutes: 35
    env:
      MINIMAX_API_KEY: ${{ secrets.MINIMAX_API_KEY }}
      LIVE_E2E_TIMEOUT_SECONDS: '1500'
      LIVE_E2E_HEARTBEAT_SECONDS: '60'
'''
new = '''  live-minimax-coding-e2e:
    if: ${{ github.event_name == 'workflow_dispatch' || github.event.pull_request.draft == false }}
    runs-on: ubuntu-latest
    timeout-minutes: 45
    env:
      MINIMAX_API_KEY: ${{ secrets.MINIMAX_API_KEY }}
      LIVE_E2E_TIMEOUT_SECONDS: '2100'
      LIVE_E2E_HEARTBEAT_SECONDS: '60'
'''
if text.count(old) != 1:
    raise SystemExit(f'expected one live provider budget block, found {text.count(old)}')
updated = text.replace(old, new, 1)
if "timeout-minutes: 45" not in updated or "LIVE_E2E_TIMEOUT_SECONDS: '2100'" not in updated:
    raise SystemExit('updated live provider budget markers are missing')
path.write_text(updated, encoding='utf-8')
print('live provider implementation-plus-review budget updated')
