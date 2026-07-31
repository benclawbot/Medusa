from pathlib import Path


def replace_once(source: str, old: str, new: str, label: str) -> str:
    if new in source:
        return source
    count = source.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one target, found {count}")
    return source.replace(old, new, 1)


cargo = Path("apps/medusa-desktop/src-tauri/Cargo.toml")
source = cargo.read_text()
source = replace_once(
    source,
    'medusa-core = { path = "../../../crates/medusa-core" }\n',
    'medusa-config = { path = "../../../crates/medusa-config" }\nmedusa-core = { path = "../../../crates/medusa-core" }\n',
    "desktop medusa-config dependency",
)
cargo.write_text(source)

config = Path("apps/medusa-desktop/src-tauri/src/config.rs")
config.write_text(r'''use std::collections::BTreeMap;

use medusa_config::{
    Config, ProviderProfile, ProviderProfileCatalog, ProviderProfileStore, credential_environment,
};
use serde::Serialize;

use crate::credentials::{CredentialStore, SystemCredentialStore};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSharedConfiguration {
    pub active_profile: String,
    pub connection: String,
    pub provider: String,
    pub model: String,
    pub effort: String,
    pub auth: String,
    pub base_url: Option<String>,
    pub configured: bool,
    pub credential_configured: bool,
}

pub(crate) struct PreparedProviderProfile {
    store: ProviderProfileStore,
    profile: ProviderProfile,
}

impl PreparedProviderProfile {
    pub(crate) fn commit(self) -> Result<(), String> {
        self.store.save(&self.profile).map_err(|error| error.to_string())
    }
}

#[tauri::command]
pub fn desktop_shared_configuration() -> Result<DesktopSharedConfiguration, String> {
    let catalog = ProviderProfileCatalog::user().map_err(|error| error.to_string())?;
    let credentials = SystemCredentialStore;
    shared_configuration(&catalog, |provider| credentials.load(provider))
}

pub(crate) fn prepare_provider_profile(
    provider: &str,
    model: &str,
    effort: &str,
) -> Result<PreparedProviderProfile, String> {
    let catalog = ProviderProfileCatalog::user().map_err(|error| error.to_string())?;
    prepare_with_catalog(&catalog, provider, model, effort)
}

fn prepare_with_catalog(
    catalog: &ProviderProfileCatalog,
    provider: &str,
    model: &str,
    effort: &str,
) -> Result<PreparedProviderProfile, String> {
    let store = catalog.active_store().map_err(|error| error.to_string())?;
    let mut profile = store.load().map_err(|error| error.to_string())?;
    profile
        .set_value("connection", connection_for_provider(provider))
        .map_err(|error| error.to_string())?;
    profile
        .set_value("provider", provider)
        .map_err(|error| error.to_string())?;
    profile
        .set_value("model", model)
        .map_err(|error| error.to_string())?;
    profile
        .set_value("reasoning", reasoning_for_effort(effort))
        .map_err(|error| error.to_string())?;
    profile
        .set_value("configured", "true")
        .map_err(|error| error.to_string())?;
    Config::load_layers_with_provider_profile(
        &profile,
        None,
        None,
        &BTreeMap::new(),
        &BTreeMap::new(),
    )
    .map_err(|error| error.to_string())?;
    Ok(PreparedProviderProfile { store, profile })
}

fn shared_configuration(
    catalog: &ProviderProfileCatalog,
    load_credential: impl FnOnce(&str) -> Result<Option<String>, String>,
) -> Result<DesktopSharedConfiguration, String> {
    let active_profile = catalog.active_name().map_err(|error| error.to_string())?;
    let profile = catalog
        .active_store()
        .map_err(|error| error.to_string())?
        .load()
        .map_err(|error| error.to_string())?;
    let environment_credential = credential_environment(&profile.provider)
        .and_then(std::env::var_os)
        .is_some_and(|value| !value.is_empty());
    let credential_configured = profile.auth == "none"
        || environment_credential
        || load_credential(&profile.provider)?.is_some();
    Ok(DesktopSharedConfiguration {
        active_profile,
        connection: profile.connection,
        provider: profile.provider,
        model: profile.model,
        effort: effort_for_reasoning(&profile.reasoning).to_owned(),
        auth: profile.auth,
        base_url: profile.base_url,
        configured: profile.configured,
        credential_configured,
    })
}

fn connection_for_provider(provider: &str) -> &'static str {
    match provider.trim().to_ascii_lowercase().as_str() {
        "openai-oauth" => "chatgpt-oauth",
        "openai" => "openai-api",
        "openai-compatible" => "openai-compatible",
        "omniroute" => "omniroute",
        "local" => "local",
        _ => "direct",
    }
}

fn reasoning_for_effort(effort: &str) -> &'static str {
    match effort.trim().to_ascii_lowercase().as_str() {
        "low" => "low",
        "high" => "high",
        "auto" | "medium" => "medium",
        _ => "medium",
    }
}

fn effort_for_reasoning(reasoning: &str) -> &'static str {
    match reasoning {
        "low" => "low",
        "high" | "maximum" => "high",
        _ => "medium",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_reads_and_writes_the_shared_active_profile() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = ProviderProfileCatalog::at(directory.path());
        prepare_with_catalog(&catalog, "openai", "gpt-5", "high")
            .expect("prepare")
            .commit()
            .expect("commit");

        let configuration = shared_configuration(&catalog, |_| Ok(Some("secret".to_owned())))
            .expect("shared configuration");
        assert_eq!(configuration.active_profile, "default");
        assert_eq!(configuration.connection, "openai-api");
        assert_eq!(configuration.provider, "openai");
        assert_eq!(configuration.model, "gpt-5");
        assert_eq!(configuration.effort, "high");
        assert!(configuration.configured);
        assert!(configuration.credential_configured);
    }

    #[test]
    fn desktop_configuration_response_never_contains_credentials() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = ProviderProfileCatalog::at(directory.path());
        let configuration = shared_configuration(&catalog, |_| Ok(Some("top-secret".to_owned())))
            .expect("shared configuration");
        let encoded = serde_json::to_string(&configuration).expect("serialize");
        assert!(!encoded.contains("top-secret"));
        assert!(!encoded.to_ascii_lowercase().contains("api_key"));
        assert!(!encoded.to_ascii_lowercase().contains("token"));
    }
}
''')

lib = Path("apps/medusa-desktop/src-tauri/src/lib.rs")
source = lib.read_text()
source = replace_once(source, "mod credentials;\n", "mod config;\nmod credentials;\n", "desktop config module")
source = replace_once(
    source,
    "use desktop_update::{desktop_update_from_main, desktop_update_status};\n",
    "use config::desktop_shared_configuration;\nuse desktop_update::{desktop_update_from_main, desktop_update_status};\n",
    "desktop config command import",
)
source = replace_once(
    source,
    "            runtime_start,\n",
    "            desktop_shared_configuration,\n            runtime_start,\n",
    "desktop config command registration",
)
lib.write_text(source)

runtime = Path("apps/medusa-desktop/src-tauri/src/runtime.rs")
source = runtime.read_text()
source = replace_once(
    source,
    '''use crate::{
    credentials::{CredentialStore, SystemCredentialStore},
''',
    '''use crate::{
    config::prepare_provider_profile,
    credentials::{CredentialStore, SystemCredentialStore},
''',
    "desktop profile preparation import",
)
old = '''    let effort = match configuration.effort.to_ascii_lowercase().as_str() {
        "low" => Effort::Low,
        "medium" => Effort::Medium,
        "high" => Effort::High,
        "auto" => Effort::Auto,
        _ => return Err("effort must be low, medium, high, or auto".to_owned()),
    };
    let provider = configuration.provider;
    let supplied_api_key = configuration.api_key.filter(|key| !key.trim().is_empty());
'''
new = '''    let effort_name = configuration.effort.to_ascii_lowercase();
    let effort = match effort_name.as_str() {
        "low" => Effort::Low,
        "medium" => Effort::Medium,
        "high" => Effort::High,
        "auto" => Effort::Auto,
        _ => return Err("effort must be low, medium, high, or auto".to_owned()),
    };
    let provider = configuration.provider;
    let model = configuration.model;
    let prepared_profile = prepare_provider_profile(&provider, &model, &effort_name)?;
    let supplied_api_key = configuration.api_key.filter(|key| !key.trim().is_empty());
'''
source = replace_once(source, old, new, "desktop validated shared profile candidate")
source = replace_once(
    source,
    '''                provider: provider.clone(),
                model: configuration.model,
                effort,
''',
    '''                provider: provider.clone(),
                model,
                effort,
''',
    "desktop model move",
)
source = replace_once(
    source,
    '''    if let Some(api_key) = supplied_api_key {
        credentials.save(&provider, &api_key)?;
    }
    Ok(())
''',
    '''    if let Some(api_key) = supplied_api_key {
        credentials.save(&provider, &api_key)?;
    }
    prepared_profile.commit()?;
    Ok(())
''',
    "desktop shared profile commit",
)
runtime.write_text(source)

runtime_ts = Path("apps/medusa-desktop/src/runtime.ts")
source = runtime_ts.read_text()
source = replace_once(
    source,
    '''export interface RuntimeStartResponse {
  runtimeId: string;
  repo: string;
}
''',
    '''export interface RuntimeStartResponse {
  runtimeId: string;
  repo: string;
}

export interface SharedConfiguration {
  activeProfile: string;
  connection: string;
  provider: string;
  model: string;
  effort: Effort;
  auth: string;
  baseUrl?: string;
  configured: boolean;
  credentialConfigured: boolean;
}
''',
    "desktop shared configuration type",
)
source = replace_once(
    source,
    '''export async function startRuntime(repo?: string): Promise<RuntimeStartResponse> {
''',
    '''export async function loadSharedConfiguration(): Promise<SharedConfiguration> {
  return invoke<SharedConfiguration>("desktop_shared_configuration");
}

export async function startRuntime(repo?: string): Promise<RuntimeStartResponse> {
''',
    "desktop shared configuration loader",
)
runtime_ts.write_text(source)

app = Path("apps/medusa-desktop/src/App.tsx")
source = app.read_text()
source = replace_once(
    source,
    '''  configureRuntime,
  pollRuntime,
''',
    '''  configureRuntime,
  loadSharedConfiguration,
  pollRuntime,
''',
    "desktop shared configuration import",
)
source = replace_once(
    source,
    '''  type RuntimeEvent,
} from "./runtime";
''',
    '''  type RuntimeEvent,
  type SharedConfiguration,
} from "./runtime";
''',
    "desktop shared configuration type import",
)
source = replace_once(
    source,
    '''interface ModelPreferences {
  provider: string;
  model: string;
  effort: Effort;
}

''',
    "",
    "remove local model preference type",
)
source = replace_once(
    source,
    '''const defaultModelPreferences: ModelPreferences = {
  provider: "minimax",
  model: "MiniMax-M2.5",
  effort: "auto",
};
''',
    "",
    "remove local model defaults",
)
start = source.index("function loadModelPreferences(): ModelPreferences {")
end = source.index("\nfunction readImage", start)
source = source[:start] + source[end + 1 :]
source = replace_once(
    source,
    '''export function App() {
  const modelPreferences = useMemo(loadModelPreferences, []);
''',
    '''export function App() {
''',
    "remove local preference memo",
)
source = replace_once(
    source,
    '''  const [provider, setProvider] = useState(modelPreferences.provider);
  const [model, setModel] = useState(modelPreferences.model);
  const [effort, setEffort] = useState<Effort>(modelPreferences.effort);
''',
    '''  const [provider, setProvider] = useState("");
  const [model, setModel] = useState("");
  const [effort, setEffort] = useState<Effort>("medium");
  const [sharedConfiguration, setSharedConfiguration] = useState<SharedConfiguration>();
''',
    "desktop shared profile state",
)
old_startup = '''  useEffect(() => {
    const previous = window.localStorage.getItem("medusa.desktop.repo");
    let disposed = false;
    const start = async () => {
      try {
        return await startRuntime(previous || undefined);
      } catch (cause) {
        if (!previous) throw cause;
        window.localStorage.removeItem("medusa.desktop.repo");
        return startRuntime();
      }
    };
    void start()
      .then((started) => {
        if (disposed) {
          void closeRuntime(started.runtimeId);
          return;
        }
        setRuntimeId(started.runtimeId);
        setRepo(started.repo);
        void configureRuntime(started.runtimeId, modelPreferences).catch((cause) => {
          if (!disposed) setError(String(cause));
        });
      })
      .catch((cause) => {
        if (!disposed) setError(String(cause));
      });
    return () => {
      disposed = true;
    };
  }, []);
'''
new_startup = '''  useEffect(() => {
    const previous = window.localStorage.getItem("medusa.desktop.repo");
    let disposed = false;
    const start = async () => {
      const configuration = await loadSharedConfiguration();
      if (!disposed) {
        setSharedConfiguration(configuration);
        setProvider(configuration.provider);
        setModel(configuration.model);
        setEffort(configuration.effort);
      }
      let started;
      try {
        started = await startRuntime(previous || undefined);
      } catch (cause) {
        if (!previous) throw cause;
        window.localStorage.removeItem("medusa.desktop.repo");
        started = await startRuntime();
      }
      await configureRuntime(started.runtimeId, {
        provider: configuration.provider,
        model: configuration.model,
        effort: configuration.effort,
      });
      return started;
    };
    void start()
      .then((started) => {
        if (disposed) {
          void closeRuntime(started.runtimeId);
          return;
        }
        setRuntimeId(started.runtimeId);
        setRepo(started.repo);
      })
      .catch((cause) => {
        if (!disposed) setError(String(cause));
      });
    return () => {
      disposed = true;
    };
  }, []);
'''
source = replace_once(source, old_startup, new_startup, "desktop startup shared profile")
source = replace_once(
    source,
    '''      window.localStorage.setItem(
        "medusa.desktop.model",
        JSON.stringify({ provider, model, effort }),
      );
      setApiKey("");
''',
    '''      const configuration = await loadSharedConfiguration();
      setSharedConfiguration(configuration);
      setProvider(configuration.provider);
      setModel(configuration.model);
      setEffort(configuration.effort);
      setApiKey("");
''',
    "desktop shared profile refresh",
)
source = replace_once(
    source,
    '''  const repoName = useMemo(() => basename(repo) || "General chat", [repo]);
''',
    '''  const credentiallessProvider = ["openai-oauth", "omniroute", "local"].includes(provider);
  const repoName = useMemo(() => basename(repo) || "General chat", [repo]);
''',
    "desktop credential route state",
)
old_settings = '''            <div className="panel-title"><Settings size={18} /><div><h2>Model settings</h2><p>Saved securely in your operating system credential manager</p></div></div>
            <label>Provider<select value={provider} onChange={(event) => setProvider(event.target.value)}><option value="minimax">MiniMax</option><option value="anthropic">Anthropic</option><option value="anthropic-compatible">Anthropic-compatible</option></select></label>
            <label>Model<input value={model} onChange={(event) => setModel(event.target.value)} /></label>
            <label>Effort<select value={effort} onChange={(event) => setEffort(event.target.value as Effort)}><option value="auto">Auto</option><option value="low">Low</option><option value="medium">Medium</option><option value="high">High</option></select></label>
            <label>API key<input type="password" value={apiKey} onChange={(event) => setApiKey(event.target.value)} placeholder="Leave blank to use the saved key" /></label>
            <button className="primary-action" onClick={applyModel} disabled={!runtimeId}>Apply configuration</button>
'''
new_settings = '''            <div className="panel-title"><Settings size={18} /><div><h2>Model settings</h2><p>Saved securely in your operating system credential manager</p><small>Shared profile: {sharedConfiguration?.activeProfile ?? "loading"}</small></div></div>
            <label>Provider<select value={provider} onChange={(event) => setProvider(event.target.value)}><option value="minimax">MiniMax</option><option value="anthropic">Anthropic</option><option value="anthropic-compatible">Anthropic-compatible</option><option value="openai">OpenAI API</option><option value="openai-oauth">ChatGPT OAuth</option><option value="openai-compatible">OpenAI-compatible</option><option value="omniroute">OmniRoute</option><option value="local">Local endpoint</option></select></label>
            <label>Model<input value={model} onChange={(event) => setModel(event.target.value)} /></label>
            <label>Effort<select value={effort} onChange={(event) => setEffort(event.target.value as Effort)}><option value="auto">Auto</option><option value="low">Low</option><option value="medium">Medium</option><option value="high">High</option></select></label>
            <label>API key<input type="password" value={apiKey} onChange={(event) => setApiKey(event.target.value)} placeholder={credentiallessProvider ? "This route does not require an API key" : "Leave blank to use the saved key"} disabled={credentiallessProvider} /></label>
            <button className="primary-action" onClick={applyModel} disabled={!runtimeId || !provider.trim() || !model.trim()}>Apply configuration</button>
'''
source = replace_once(source, old_settings, new_settings, "desktop shared profile settings UI")
app.write_text(source)

test = Path("apps/medusa-desktop/src/App.test.tsx")
source = test.read_text()
source = replace_once(
    source,
    '''import { commandSuggestions, pollRuntime, runRuntimeCommand, startRuntime } from "./runtime";
''',
    '''import { commandSuggestions, loadSharedConfiguration, pollRuntime, runRuntimeCommand, startRuntime } from "./runtime";
''',
    "desktop shared configuration test import",
)
source = replace_once(
    source,
    '''    startRuntime: vi.fn(),
''',
    '''    loadSharedConfiguration: vi.fn().mockResolvedValue({
      activeProfile: "default",
      connection: "direct",
      provider: "minimax",
      model: "MiniMax-M3",
      effort: "medium",
      auth: "api-key",
      configured: false,
      credentialConfigured: false,
    }),
    startRuntime: vi.fn(),
''',
    "desktop shared configuration test mock",
)
source = replace_once(
    source,
    '''  vi.mocked(startRuntime).mockReset();
''',
    '''  vi.mocked(loadSharedConfiguration).mockReset().mockResolvedValue({
    activeProfile: "default",
    connection: "direct",
    provider: "minimax",
    model: "MiniMax-M3",
    effort: "medium",
    auth: "api-key",
    configured: false,
    credentialConfigured: false,
  });
  vi.mocked(startRuntime).mockReset();
''',
    "desktop shared configuration test reset",
)
source = replace_once(
    source,
    '''  await waitFor(() => expect(startRuntime).toHaveBeenCalledWith(undefined));
''',
    '''  await waitFor(() => expect(loadSharedConfiguration).toHaveBeenCalled());
  await waitFor(() => expect(startRuntime).toHaveBeenCalledWith(undefined));
  expect(window.localStorage.getItem("medusa.desktop.model")).toBeNull();
''',
    "desktop shared profile startup assertion",
)
test.write_text(source)
