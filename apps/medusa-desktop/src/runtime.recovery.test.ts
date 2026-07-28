import { invoke } from "@tauri-apps/api/core";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { performRecoveryAction, type RecoveryView } from "./runtime";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mockedInvoke = vi.mocked(invoke);

const recovery: RecoveryView = {
  sessionId: "session-1",
  health: "NeedsConfirmation",
  lastDurableStep: "step",
  currentRepositoryFingerprint: "current",
  verification: "Incomplete",
  approvalsMustBeReestablished: false,
  containmentMustBeReestablished: false,
  checkpoints: [{
    id: "cp-1",
    sequence: 1,
    createdAtUnixMs: 1,
    taskStep: "step",
    reason: "reason",
    repositoryFingerprint: "checkpoint",
    verification: "Incomplete",
    provenance: "execution-checkpoint/v1",
    integrityVerified: true,
  }],
  selectedPreview: {
    checkpointId: "cp-1",
    files: [{ path: "src/lib.rs", kind: "modified", wouldOverwriteUncommittedWork: true }],
    unresolvedRisks: ["drift"],
    repositoryMatchesCheckpointBase: false,
  },
  actions: [],
  warnings: [],
};

describe("performRecoveryAction", () => {
  beforeEach(() => mockedInvoke.mockReset());

  it("passes selected checkpoint preflight evidence to the guarded Tauri command", async () => {
    await performRecoveryAction("runtime-1", recovery, "restoreCheckpoint", "cp-1", true);

    expect(mockedInvoke).toHaveBeenCalledWith("runtime_recovery_action", {
      runtimeId: "runtime-1",
      request: expect.objectContaining({
        checkpointId: "cp-1",
        confirmedDestructiveEffects: true,
        checkpointIntegrityVerified: true,
        repositoryPreconditionsVerified: false,
        conflictingUncommittedPaths: ["src/lib.rs"],
        unresolvedRisks: ["drift"],
      }),
    });
  });
});
