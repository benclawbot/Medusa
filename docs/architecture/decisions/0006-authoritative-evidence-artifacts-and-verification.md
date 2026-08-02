# ADR 0006: Authoritative evidence, artifacts, and changed-component verification

- Status: Accepted
- Date: 2026-08-02
- Issue: #650

## Context

Architecture v2 now keeps mutation commits isolated until durable review, independent verification, authorization, integration, and reconciliation. However, several inputs to those gates remain free-form strings or coarse repository-wide verification results. Changed paths can lose rename, deletion, generated-file, and package ownership semantics; large logs and diffs lack stable retrievable identities; and a conclusion can be accepted without proving that its referenced artifact was resolved and read.

## Decision

Review, scheduler dependencies, verification, authorization, and integration will consume one typed evidence authority.

Evidence records distinguish observations, inferences, claims, and decisions. Every record is bound to a repository fingerprint, commit, producer, timestamp, verification status, and one or more resolvable sources. Sources may identify path and line ranges, command receipts, durable artifact IDs, content hashes, or prior evidence. Stale, missing, contradictory, unread, or unresolvable sources fail closed and cannot satisfy a dependency or integration gate.

Complete bounded artifacts are stored under stable content-addressed IDs with media type, byte length, hash, producer, creation time, paging metadata, binary awareness, range reads, text search, and durable read receipts. A dependent conclusion records which artifact ranges were actually read. Non-empty output is never treated as semantic validity by itself.

Changed-component scope preserves additions, modifications, renames, deletions, generated files, package ownership, and effective UI impact from preparation through review, verification receipts, authorization, integration, and reconciliation.

An extensible verification planner maps that exact scope to required checks for formatting, linting, type checking, unit and integration tests, builds, browser behavior, accessibility, packaging, security, and artifact semantics. Adapters support repository-defined commands and common Cargo, npm, pnpm, yarn, bun, pytest, Go, Maven/Gradle, .NET, and CMake layouts. Effective UI changes require real browser behavior checks unless a durable reviewed exemption is bound to the exact scope. Missing adapters, unresolved ownership, failed checks, corrupt artifacts, or incomplete coverage fail closed.

Direct and isolated mutation paths use the same planner and evidence authority for the same changed files. Verification receipts contain exact scope, planner inputs, selected checks, exemptions, command and artifact outputs, coverage, and terminal decision.

## Consequences

- Free-form worker summaries cannot authorize integration.
- Large logs, diffs, and binary artifacts remain retrievable by stable ID and bounded range.
- Reviewers can prove which evidence was resolved and read before accepting a claim.
- Renames, deletions, generated files, ownership, and UI impact survive every transaction boundary.
- Equivalent direct and isolated changes select equivalent verification plans.
- UI, packaging, and generated-artifact changes fail closed without semantic checks.
- Legacy coarse verification and untyped evidence paths are removed only after semantic conformance gates pass.
