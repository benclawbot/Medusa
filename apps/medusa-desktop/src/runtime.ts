import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import * as legacy from "./runtimeLegacy";

export * from "./runtimeLegacy";

const RUNTIME_WAKE_EVENT = "medusa-runtime-wakeup";
const FALLBACK_POLL_MS = 1500;

interface WakeState {
  pending: boolean;
  lastPollAt: number;
  starting?: Promise<void>;
  unlisten?: UnlistenFn;
}

const wakeStates = new Map<string, WakeState>();

function wakeState(runtimeId: string): WakeState {
  let state = wakeStates.get(runtimeId);
  if (!state) {
    state = { pending: true, lastPollAt: 0 };
    wakeStates.set(runtimeId, state);
  }
  return state;
}

function markRuntimeWake(runtimeId: string): void {
  wakeState(runtimeId).pending = true;
}

async function ensureRuntimeWakeups(runtimeId: string): Promise<void> {
  const state = wakeState(runtimeId);
  if (state.unlisten) return;
  if (state.starting) return state.starting;

  state.starting = (async () => {
    const unlisten = await listen<string>(RUNTIME_WAKE_EVENT, (event) => {
      if (event.payload === runtimeId) markRuntimeWake(runtimeId);
    });
    try {
      await invoke("runtime_begin_wakeups", { runtimeId });
      state.unlisten = unlisten;
    } catch (error) {
      unlisten();
      throw error;
    }
  })().finally(() => {
    state.starting = undefined;
  });
  return state.starting;
}

function disposeRuntimeWakeups(runtimeId: string): void {
  const state = wakeStates.get(runtimeId);
  state?.unlisten?.();
  wakeStates.delete(runtimeId);
}

/**
 * Start the durable runtime first, then subscribe to backend replay wakeups. Browser-only test
 * harnesses do not expose the native event command, so failure to install the optimization keeps
 * the low-frequency fallback path rather than failing startup.
 */
export async function startRuntime(repo?: string): Promise<legacy.RuntimeStartResponse> {
  const response = await legacy.startRuntime(repo);
  markRuntimeWake(response.runtimeId);
  void ensureRuntimeWakeups(response.runtimeId).catch(() => undefined);
  return response;
}

export async function closeRuntime(runtimeId: string): Promise<void> {
  disposeRuntimeWakeups(runtimeId);
  await legacy.closeRuntime(runtimeId);
}

export async function submitRuntime(
  runtimeId: string,
  draft: legacy.DesktopPromptDraft,
): Promise<legacy.SubmitDisposition> {
  const disposition = await legacy.submitRuntime(runtimeId, draft);
  markRuntimeWake(runtimeId);
  return disposition;
}

export async function runRuntimeCommand(runtimeId: string, input: string): Promise<void> {
  await legacy.runRuntimeCommand(runtimeId, input);
  markRuntimeWake(runtimeId);
}

export async function cancelRuntime(runtimeId: string): Promise<boolean> {
  const requested = await legacy.cancelRuntime(runtimeId);
  markRuntimeWake(runtimeId);
  return requested;
}

export async function performRecoveryAction(
  runtimeId: string,
  recovery: legacy.RecoveryView,
  operation: legacy.RecoveryOperation,
  checkpointId?: string,
  confirmedDestructiveEffects = false,
): Promise<void> {
  await legacy.performRecoveryAction(
    runtimeId,
    recovery,
    operation,
    checkpointId,
    confirmedDestructiveEffects,
  );
  markRuntimeWake(runtimeId);
}

/**
 * Drain canonical runtime events when the backend says replay advanced. The existing caller may
 * ask frequently while a turn is busy, but those calls are local no-ops unless a wake is pending.
 * A 1.5 s fallback protects against a lost native event or older backend.
 */
export async function pollRuntime(runtimeId: string): Promise<legacy.RuntimeEvent[]> {
  const state = wakeState(runtimeId);
  void ensureRuntimeWakeups(runtimeId).catch(() => undefined);
  const now = Date.now();
  if (!state.pending && now - state.lastPollAt < FALLBACK_POLL_MS) {
    return [];
  }

  state.pending = false;
  state.lastPollAt = now;
  try {
    return await legacy.pollRuntime(runtimeId);
  } catch (error) {
    // A failed drain may still have unread durable replay. Keep the next call eligible instead of
    // suppressing it until the fallback deadline.
    state.pending = true;
    throw error;
  }
}

export const runtimeWakeupPolicy = {
  fallbackPollMs: FALLBACK_POLL_MS,
} as const;
