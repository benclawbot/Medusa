# Integration validation

This branch is rebuilt from the current `main` branch and contains the stacked runtime crates selected for canonical CI validation.

Generated workspace metadata must remain synchronized with every crate manifest before merge.

The final integration run validates all workspace manifests against the committed lockfile, ownership-safe transaction coordination, and strict workspace lint policy.
