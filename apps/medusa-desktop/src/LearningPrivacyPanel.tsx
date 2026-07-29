import { useEffect, useState } from "react";
import { Download, ShieldCheck } from "lucide-react";
import {
  exportLearningAudit,
  loadLearningPrivacy,
  updateLearningPrivacy,
  type LearningPrivacySettings,
} from "./engineeringApi";

export function LearningPrivacyPanel({ repo }: { repo: string }) {
  const [settings, setSettings] = useState<LearningPrivacySettings>();
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string>();

  useEffect(() => {
    if (repo) {
      void loadLearningPrivacy(repo).then(setSettings).catch((error) => setMessage(String(error)));
    }
  }, [repo]);

  if (!settings) return null;

  const toggle = async (key: keyof LearningPrivacySettings, value: boolean) => {
    const next = { ...settings, [key]: value };
    setBusy(true);
    try {
      setSettings(await updateLearningPrivacy(repo, next));
      setMessage("Learning privacy settings saved.");
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  };

  const controls: Array<[keyof LearningPrivacySettings, string]> = [
    ["captureEnabled", "Capture learning signals"],
    ["repositoryPersistence", "Persist repository learning"],
    ["crossRepositoryReuse", "Allow cross-repository reuse"],
    ["telemetryEnabled", "Share learning telemetry"],
    ["automaticProposals", "Generate proposals automatically"],
  ];

  return <section className="engineering-card privacy-card" aria-labelledby="learning-privacy-heading">
    <div className="card-head"><div><span className="eyebrow">Privacy and control</span><h3 id="learning-privacy-heading">Learning settings</h3></div><ShieldCheck size={18}/></div>
    <div className="privacy-grid">{controls.map(([key,label]) => <label key={key}><input type="checkbox" checked={settings[key]} disabled={busy} onChange={(event) => void toggle(key,event.target.checked)}/><span>{label}</span></label>)}</div>
    <button disabled={busy} onClick={async()=>{setBusy(true);try{setMessage(`Audit exported to ${await exportLearningAudit(repo)}`);}catch(error){setMessage(String(error));}finally{setBusy(false);}}}><Download size={14}/>Export tamper-evident audit</button>
    {message && <p role="status">{message}</p>}
  </section>;
}
