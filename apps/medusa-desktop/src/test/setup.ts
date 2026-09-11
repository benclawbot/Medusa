import "@testing-library/jest-dom/vitest";

// Node 25 can reserve the `window.localStorage` name for its own Web Storage
// implementation and leave it unavailable unless the process is started with
// --localstorage-file. The desktop uses localStorage for harmless UI/session
// preferences, so provide a small in-memory implementation for jsdom tests.
if (typeof window !== "undefined" && !window.localStorage) {
  const values = new Map<string, string>();
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, String(value)),
      removeItem: (key: string) => values.delete(key),
      clear: () => values.clear(),
      key: (index: number) => Array.from(values.keys())[index] ?? null,
      get length() {
        return values.size;
      },
    } satisfies Storage,
  });
}
