import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { RecoveryPanel } from "./RecoveryPanel";
import type { RecoveryView } from "./runtime";

const recovery: RecoveryView = {
  sessionId: "session-1",
  health: "NeedsConfirmation",
  lastDurableStep: "edit source",
  interruptedOperation: "verification",
  currentRepositoryFingerprint: "current-fingerprint",
  verification: "Incomplete",
  approvalsMustBeReestablished: true,
  containmentMustBeReestablished: true,
  checkpoints: [{
    id: "cp-1",
    sequence: 1,
    createdAtUnixMs: 1_700_000_000_000,
    taskStep: "edit source",
    reason: "before verification",
    repositoryFingerprint: "checkpoint-fingerprint",
    verification: "Incomplete",
    provenance: "execution-checkpoint/v1",
    integrityVerified: true,
  }],
  selectedPreview: {
    checkpointId: "cp-1",
    files: [{ path: "src/lib.rs", kind: "modified", wouldOverwriteUncommittedWork: true }],
    unresolvedRisks: ["Repository drift detected"],
    repositoryMatchesCheckpointBase: false,
  },
  actions: [
    { operation: "inspect", enabled: true, requiresConfirmation: false, reason: "" },
    { operation: "resume", enabled: true, requiresConfirmation: false, reason: "" },
    { operation: "restoreCheckpoint", enabled: true, requiresConfirmation: true, reason: "" },
    { operation: "retryVerification", enabled: true, requiresConfirmation: false, reason: "" },
    { operation: "abandon", enabled: true, requiresConfirmation: false, reason: "" },
  ],
  warnings: ["Review repository drift before continuing."],
};

describe("RecoveryPanel", () => {
  it("shows recovery evidence and gates destructive restore behind confirmation", async () => {
    const user = userEvent.setup();
    const onAction = vi.fn().mockResolvedValue(undefined);
    render(<RecoveryPanel recovery={recovery} onAction={onAction} />);

    expect(screen.getByRole("heading", { name: "Recovery required" })).toBeInTheDocument();
    expect(screen.getByText("src/lib.rs")).toBeInTheDocument();
    const restore = screen.getByRole("button", { name: /restore/i });
    expect(restore).toBeDisabled();

    await user.click(screen.getByRole("checkbox"));
    expect(restore).toBeEnabled();
    await user.click(restore);

    expect(onAction).toHaveBeenCalledWith("restoreCheckpoint", "cp-1", true);
  });
});
