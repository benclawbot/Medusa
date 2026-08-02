#!/usr/bin/env python3
from pathlib import Path

source = Path('scripts/fix-650-trusted-rustfmt-preparation.py')
text = source.read_text(encoding='utf-8')
start = text.index('# Direct mutation uses the same trusted exact-file preparation.')
end = text.index('# Isolated mutation prepares the exact changed files', start)
replacement = '''# Direct mutation uses the same trusted exact-file preparation.
engine = Path('crates/medusa-agent/src/engine.rs')
text = engine.read_text(encoding='utf-8')
old_import = '    verification_authority::authoritative_verification_for_paths,\\n'
new_import = """    verification_authority::{
        authoritative_verification_for_paths, prepare_paths_for_verification,
    },
"""
if text.count(old_import) != 1:
    raise SystemExit(f'engine import anchor count={text.count(old_import)}')
text = text.replace(old_import, new_import, 1)
call = """            let mut verification = authoritative_verification_for_paths(
                &session.repo,
                &successful_mutation_paths(session),
            )?;
"""
replacement = """            let changed_paths = successful_mutation_paths(session);
            prepare_paths_for_verification(&session.repo, &changed_paths)?;
            let mut verification =
                authoritative_verification_for_paths(&session.repo, &changed_paths)?;
"""
if text.count(call) != 1:
    raise SystemExit(f'direct verification call anchor count={text.count(call)}')
engine.write_text(text.replace(call, replacement, 1), encoding='utf-8')

'''
patched = text[:start] + replacement + text[end:]
patched = patched.replace(
    'ErrorCode::VerificationFailed,\n            ErrorCategory::Validation,',
    'ErrorCode::ToolExecutionFailed,\n            ErrorCategory::Execution,',
)
Path('/tmp/fix-650-trusted-rustfmt-preparation.py').write_text(
    patched,
    encoding='utf-8',
)
