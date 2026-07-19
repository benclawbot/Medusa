# Compatibility and Versioning

Medusa independently versions the workspace packages, wire protocol, and configuration schema.

## Protocol

- Major mismatches are incompatible.
- A consumer accepts the same major and a peer minor version less than or equal to its own.
- New optional fields require a minor increment and defaults.
- Removing or changing required fields requires a major increment.
- Unknown fields are rejected on integrity-sensitive envelopes.

## Configuration

- Version `1` is the initial schema.
- Unknown fields are rejected to prevent misspelled safety settings.
- Migrations must be explicit, tested, and reversible.
- Precedence is CLI, environment, project, user, then built-in defaults.
