from pathlib import Path

path = Path("crates/medusa-agent/src/lib.rs")
text = path.read_text(encoding="utf-8")

old = "mod verification;\nmod verification_authority;\n"
new = "mod verification;\npub mod verification_dag;\nmod verification_authority;\n"
if text.count(old) != 1:
    raise SystemExit(f"module anchor count: {text.count(old)}")
text = text.replace(old, new, 1)

old = "pub use verification::VerificationResult;\npub use verification_authority::{\n"
new = "pub use verification::VerificationResult;\npub use verification_dag::{\n    VerificationAuthority, VerificationDag, VerificationInputKey, VerificationNode,\n    VerificationNodeState, VerificationReceipt,\n};\npub use verification_authority::{\n"
if text.count(old) != 1:
    raise SystemExit(f"export anchor count: {text.count(old)}")
text = text.replace(old, new, 1)

path.write_text(text, encoding="utf-8")
