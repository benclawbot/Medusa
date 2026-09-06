import { act, render, screen } from "@testing-library/react";
import { useState } from "react";
import { describe, expect, it } from "vitest";
import { DesktopSlotsProvider, useDesktopSlots } from "./DesktopSlots";

function SlotHarness() {
  const { todoTarget, updateTarget, todoRef, updateRef } = useDesktopSlots();
  const [chat, setChat] = useState(false);
  const [settings, setSettings] = useState(false);
  return (
    <>
      <button onClick={() => setChat((value) => !value)}>toggle chat</button>
      <button onClick={() => setSettings((value) => !value)}>toggle settings</button>
      <output aria-label="todo target">{todoTarget ? "mounted" : "absent"}</output>
      <output aria-label="update target">{updateTarget ? "mounted" : "absent"}</output>
      {chat && <div ref={todoRef} data-medusa-todos />}
      {settings && <div ref={updateRef} data-medusa-updates />}
    </>
  );
}

describe("DesktopSlotsProvider", () => {
  it("tracks late mounts and clears/re-registers targets when panels remount", () => {
    render(<DesktopSlotsProvider><SlotHarness /></DesktopSlotsProvider>);
    expect(screen.getByLabelText("todo target")).toHaveTextContent("absent");
    expect(screen.getByLabelText("update target")).toHaveTextContent("absent");

    act(() => screen.getByRole("button", { name: "toggle chat" }).click());
    expect(screen.getByLabelText("todo target")).toHaveTextContent("mounted");
    act(() => screen.getByRole("button", { name: "toggle chat" }).click());
    expect(screen.getByLabelText("todo target")).toHaveTextContent("absent");
    act(() => screen.getByRole("button", { name: "toggle chat" }).click());
    expect(screen.getByLabelText("todo target")).toHaveTextContent("mounted");

    act(() => screen.getByRole("button", { name: "toggle settings" }).click());
    expect(screen.getByLabelText("update target")).toHaveTextContent("mounted");
    act(() => screen.getByRole("button", { name: "toggle settings" }).click());
    expect(screen.getByLabelText("update target")).toHaveTextContent("absent");
  });
});
