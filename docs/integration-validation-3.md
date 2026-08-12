This branch was created from current main for clean stacked-runtime integration.

Production panic-audit findings must propagate serialization errors instead of panicking.

Worker transaction fingerprinting follows the same non-panicking error-propagation rule.

Memory consolidation fingerprinting also propagates serialization errors.

Repository snapshot and memory writeback fingerprinting are non-panicking and error-aware.

The final serialization repair is verified against the exact current source blocks before commit.
