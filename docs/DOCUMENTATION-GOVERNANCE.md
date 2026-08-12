# Documentation governance

Repository documentation has one of two explicit dispositions in `documentation-inventory.json`: `current` describes active behavior, policy, setup, or evidence; `historical` preserves implementation or decision context and carries the standard historical banner so it cannot be mistaken for current guidance. Obsolete documents without a continuing audience are removed.

The root `README.md` is the product and installation entry point. `docs/README.md` routes maintainers to current operational authorities. `docs/architecture/INDEX.md` and its machine-readable baseline own architecture certification, `docs/CAPABILITY-CLAIMS.json` owns the legacy capability ledger, `docs/provider-support.json` owns provider and live-dogfood status, and `release/keys/keyring.json` owns public release-key lifecycle state. Prose may explain these authorities but may not silently replace them.

`scripts/check-documentation.py` enumerates every tracked Markdown file, validates local links, and compares the exact file digest and disposition with the reviewed inventory. A documentation change therefore requires an explicit review followed by `python scripts/check-documentation.py --write`; CI rejects unreviewed additions, removals, edits, broken local links, or missing governance links.

Historical documents must begin with a blockquote containing `Historical record —` and point readers back to a current index or authority. Removing the banner promotes the document to current guidance and requires the same inventory review.

Generated documents identify their source in the document itself and are checked by their owning generator or validator. In particular, `docs/PROVIDER-SUPPORT.md` is rendered from `docs/provider-support.json` and must not be edited independently.
