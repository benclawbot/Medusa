import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { OpenAiRealtimeLiveEvidence } from "./OpenAiRealtimeLiveEvidence";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const mockedInvoke = vi.mocked(invoke);

afterEach(() => {
  cleanup();
  mockedInvoke.mockReset();
});

function configureInvoke(provider: string, available: boolean, reason?: string) {
  mockedInvoke.mockImplementation(async (command) => {
    if (command === "desktop_shared_configuration") {
      return {
        connection: provider === "openai-oauth" ? "chatgpt-oauth" : "direct",
        provider,
        model: provider === "openai-oauth" ? "gpt-realtime" : "MiniMax-M3",
        auth: provider === "openai-oauth" ? "none" : "api-key",
        configured: true,
        credentialConfigured: provider === "openai-oauth",
      } as never;
    }
    if (command === "desktop_realtime_capability") {
      return {
        available,
        reason,
        supportsInputAudio: available,
        supportsOutputAudio: available,
        supportsBargeIn: available,
      } as never;
    }
    throw new Error(`unexpected command: ${command}`);
  });
}

describe("OpenAI Realtime live evidence", () => {
  it("does not offer a live run while another provider is active", async () => {
    configureInvoke("minimax", false, "Realtime is unavailable for minimax");

    render(<OpenAiRealtimeLiveEvidence />);

    await waitFor(() => {
      expect(screen.getByRole("dialog")).toHaveTextContent(
        "ChatGPT OAuth is required",
      );
    });
    expect(
      screen.getByRole("button", { name: "Start 45-second live evidence" }),
    ).toBeDisabled();
  });

  it("enables the live run only after OAuth capability preflight passes", async () => {
    configureInvoke("openai-oauth", true);

    render(<OpenAiRealtimeLiveEvidence />);

    await waitFor(() => {
      expect(
        screen.getByRole("button", { name: "Start 45-second live evidence" }),
      ).toBeEnabled();
    });
    expect(screen.getByText(/ChatGPT OAuth is configured/)).toBeInTheDocument();
  });
});
