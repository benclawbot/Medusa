# Medusa: first successful session

This guide is for someone evaluating Medusa for the first time. Read the [security model](SECURITY-MODEL.md) before allowing mutations in sensitive repositories.

## 1. Install and diagnose

Use the pinned toolchain or prebuilt release instructions in [README](../README.md#installation).

```bash
medusa doctor
medusa update --check
```

The update check is read-only. Configure a supported provider as explained in [provider support](PROVIDER-SUPPORT.md). Do not paste provider credentials into chat transcripts or commits.

## 2. Start read-only

Run Medusa over a disposable project or a repository containing no sensitive secrets:

```bash
cd /path/to/sandbox-repository
medusa run "Summarize the architecture and identify missing tests; do not modify files"
```

Inspect the proposed plan and evidence, rather than assuming an agent response constitutes verified completion.

## 3. Try a guarded change

Create a disposable Git worktree/branch first, then run:

```bash
medusa --prompt "Add a minimal regression test for one bug; verify it before proposing integration"
```

Review the mutation plan, tool permissions, verification output, and accepted Git diff. Approve integration only when the evidence matches your intended scope.

## 4. Recover and inspect

```bash
medusa --continue
medusa doctor
```

State under `.medusa/` records durable sessions, approvals and recovery authority. Never delete it to clear an interrupted run without first checking the recovery instructions.

## Distribution and platform limitations

Desktop release packages are **currently unsigned at the OS level**. Verify upstream release checksums and the release provenance before installation. Future signing requires platform certificates, secure identity provisioning, secure CI secret handling, reproducible validation, and verification on actual target machines; none of those are claimed by this quickstart.

See [current limitations](../README.md#current-limitations) and [desktop distribution](DESKTOP-DISTRIBUTION.md).
