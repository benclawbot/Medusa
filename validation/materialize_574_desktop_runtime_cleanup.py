from pathlib import Path

root = Path(__file__).resolve().parents[1]
app_path = root / "apps/medusa-desktop/src/App.tsx"
test_path = root / "apps/medusa-desktop/src/App.test.tsx"

app = app_path.read_text()
helper_marker = "export function App() {\n"
helper = '''async function configureStartedRuntime(
  started: Awaited<ReturnType<typeof startRuntime>>,
  configuration: { provider: string; model: string; effort: Effort },
): Promise<Awaited<ReturnType<typeof startRuntime>>> {
  try {
    await configureRuntime(started.runtimeId, configuration);
    return started;
  } catch (cause) {
    try {
      await closeRuntime(started.runtimeId);
    } catch (cleanupCause) {
      throw new Error(
        `Runtime configuration failed (${String(cause)}); cleanup also failed (${String(cleanupCause)}).`,
      );
    }
    throw cause;
  }
}

'''
if helper_marker not in app:
    raise SystemExit("App helper marker missing")
app = app.replace(helper_marker, helper + helper_marker, 1)

startup = '''      await configureRuntime(started.runtimeId, {
        provider: configuration.provider,
        model: configuration.model,
        effort: configuration.effort,
      });
      return started;
'''
startup_replacement = '''      return configureStartedRuntime(started, {
        provider: configuration.provider,
        model: configuration.model,
        effort: configuration.effort,
      });
'''
if startup not in app:
    raise SystemExit("Initial runtime configuration block missing")
app = app.replace(startup, startup_replacement, 1)

project = '''      const started = await startRuntime(selected);
      await configureRuntime(started.runtimeId, { provider, model, effort });
'''
project_replacement = '''      const started = await configureStartedRuntime(await startRuntime(selected), {
        provider,
        model,
        effort,
      });
'''
if project not in app:
    raise SystemExit("Project runtime configuration block missing")
app = app.replace(project, project_replacement, 1)

general = '''      const started = await startRuntime();
      await configureRuntime(started.runtimeId, { provider, model, effort });
'''
general_replacement = '''      const started = await configureStartedRuntime(await startRuntime(), {
        provider,
        model,
        effort,
      });
'''
if general not in app:
    raise SystemExit("General runtime configuration block missing")
app = app.replace(general, general_replacement, 1)
app_path.write_text(app)

test = test_path.read_text()
old_import = 'import { commandSuggestions, loadSharedConfiguration, pollRuntime, runRuntimeCommand, startRuntime } from "./runtime";\n'
new_import = '''import {
  closeRuntime,
  commandSuggestions,
  configureRuntime,
  loadSharedConfiguration,
  pollRuntime,
  runRuntimeCommand,
  startRuntime,
} from "./runtime";
'''
if old_import not in test:
    raise SystemExit("Runtime test import missing")
test = test.replace(old_import, new_import, 1)

reset_marker = '''  vi.mocked(startRuntime).mockReset();
  vi.mocked(commandSuggestions).mockReset().mockResolvedValue([]);
'''
reset_replacement = '''  vi.mocked(startRuntime).mockReset();
  vi.mocked(closeRuntime).mockReset().mockResolvedValue(undefined);
  vi.mocked(configureRuntime).mockReset().mockResolvedValue(undefined);
  vi.mocked(commandSuggestions).mockReset().mockResolvedValue([]);
'''
if reset_marker not in test:
    raise SystemExit("Runtime reset marker missing")
test = test.replace(reset_marker, reset_replacement, 1)

test_marker = 'it("presents API keys as persistent OS-managed credentials", async () => {\n'
cleanup_test = '''it("closes a newly started runtime when shared configuration is rejected", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-orphan", repo: "" });
  vi.mocked(configureRuntime).mockRejectedValueOnce(new Error("configuration rejected"));

  render(<App />);

  await waitFor(() =>
    expect(closeRuntime).toHaveBeenCalledWith("runtime-orphan"),
  );
  expect(screen.getByText(/configuration rejected/i)).toBeInTheDocument();
});

'''
if test_marker not in test:
    raise SystemExit("Test insertion marker missing")
test = test.replace(test_marker, cleanup_test + test_marker, 1)
test_path.write_text(test)
