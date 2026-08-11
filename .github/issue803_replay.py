from pathlib import Path
import subprocess

source = Path('.github/workflows/issue803-ux.yml').read_text().splitlines()
start = next(i for i, line in enumerate(source) if line.strip() == 'run: |') + 1
block = []
for line in source[start:]:
    if line.startswith('          '):
        block.append(line[10:])
    elif not line.strip():
        block.append('')
    else:
        break
script = '\n'.join(block) + '\n'

patch = Path('/tmp/patch_render.py')
patch.write_text('''from pathlib import Path\np = Path("crates/medusa-tui/src/render.rs")\ntext = p.read_text()\nold = "        app::SettingsPage::BaseUrl => \\\"Base URL\\\",\\n        app::SettingsPage::Status => \\\"Status\\\",\\n    };\\n"\nnew = "        app::SettingsPage::BaseUrl => \\\"Base URL\\\",\\n        app::SettingsPage::Status => \\\"Status\\\",\\n        app::SettingsPage::Review => \\\"Review changes\\\",\\n    };\\n"\nassert old in text, "missing settings page match"\ntext = text.replace(old, new, 1)\nmarker = "    if page == app::SettingsPage::BaseUrl {\\n"\nreview = "    if page == app::SettingsPage::Review {\\n        let review = modal.settings_review_lines();\\n        if review.is_empty() {\\n            lines.push(StyledLine::new(\\\"No staged changes.\\\", Color::Grey));\\n        } else {\\n            lines.push(StyledLine::new(\\\"Pending non-secret changes:\\\", Color::White));\\n            for change in review {\\n                lines.push(StyledLine::new(format!(\\\"  {change}\\\"), Color::Grey));\\n            }\\n            lines.push(StyledLine::new(\\n                \\\"Enter applies all staged changes atomically · Esc returns without applying.\\\",\\n                Color::DarkGrey,\\n            ));\\n        }\\n        return lines;\\n    }\\n"\nassert marker in text, "missing base-url render marker"\np.write_text(text.replace(marker, review + marker, 1))\n''')
script = script.replace('cargo fmt --all', 'python /tmp/patch_render.py\ncargo fmt --all', 1)
script = script.replace(
    'rm .github/workflows/issue803-ux.yml',
    'rm .github/workflows/issue803-ux.yml .github/workflows/issue803-ux-fix.yml .github/workflows/issue803-ux-fix2.yml .github/workflows/issue803-run.yml .github/issue803_replay.py',
    1,
)
Path('/tmp/issue803-ux.sh').write_text(script)
subprocess.run(['bash', '/tmp/issue803-ux.sh'], check=True)
