import {
  createContext,
  type PropsWithChildren,
  type RefCallback,
  useContext,
  useMemo,
  useState,
} from "react";

interface DesktopSlotsValue {
  todoTarget: HTMLDivElement | null;
  updateTarget: HTMLDivElement | null;
  todoRef: RefCallback<HTMLDivElement>;
  updateRef: RefCallback<HTMLDivElement>;
}

const DesktopSlotsContext = createContext<DesktopSlotsValue | undefined>(undefined);

export function DesktopSlotsProvider({ children }: PropsWithChildren) {
  const [todoTarget, setTodoTarget] = useState<HTMLDivElement | null>(null);
  const [updateTarget, setUpdateTarget] = useState<HTMLDivElement | null>(null);
  const value = useMemo<DesktopSlotsValue>(() => ({
    todoTarget,
    updateTarget,
    todoRef: setTodoTarget,
    updateRef: setUpdateTarget,
  }), [todoTarget, updateTarget]);

  return <DesktopSlotsContext.Provider value={value}>{children}</DesktopSlotsContext.Provider>;
}

export function useDesktopSlots(): DesktopSlotsValue {
  const value = useContext(DesktopSlotsContext);
  if (!value) {
    throw new Error("Desktop auxiliary surfaces must be rendered inside DesktopSlotsProvider.");
  }
  return value;
}
