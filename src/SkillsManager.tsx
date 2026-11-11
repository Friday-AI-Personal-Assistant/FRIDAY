import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type Skill = {
  id: string;
  name: string;
  description: string;
  permissions: string[];
  installed: boolean;
  enabled: boolean;
};

const ICONS: Record<string, string> = {
  "web-research": "⌕",
  "pdf-reader": "▤",
  "file-assistant": "▣",
  "coding-assistant": "</>"
};

export default function SkillsManager() {
  const desktop = "__TAURI_INTERNALS__" in window;
  const [skills, setSkills] = useState<Skill[]>([]);
  const [loading, setLoading] = useState(true);
  const [busyId, setBusyId] = useState("");
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");

  const refresh = useCallback(async () => {
    if (!desktop) { setLoading(false); return; }
    try {
      const result = await invoke<Skill[]>("skills_catalog");
      setSkills(result);
      setError("");
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [desktop]);

  useEffect(() => { void refresh(); }, [refresh]);

  async function runAction(skill: Skill, action: "install" | "enable" | "disable" | "uninstall") {
    if (!desktop || busyId) return;
    if (action === "uninstall" && !window.confirm(`Uninstall ${skill.name} from this computer?`)) return;
    setBusyId(skill.id);
    setError("");
    setNotice("");
    try {
      let result: string;
      if (action === "install") result = await invoke<string>("install_skill", { skillId: skill.id });
      else if (action === "uninstall") result = await invoke<string>("uninstall_skill", { skillId: skill.id });
      else result = await invoke<string>("set_skill_enabled", { skillId: skill.id, enabled: action === "enable" });
      setNotice(result);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyId("");
    }
  }

  return <section className="skills-manager">
    <div className="skills-intro">
      <div><div className="eyebrow">MODULAR CAPABILITIES</div><h2>Skills Store</h2>
        <p>Install only the capabilities you want. Installed skills stay on disk and do not run in the background.</p>
      </div>
      <button className="secondary-button" onClick={() => void refresh()} disabled={!desktop || loading || !!busyId}>Refresh</button>
    </div>

    {!desktop && <div className="skills-message">The Skills Store works in the installed FRIDAY desktop app. This browser preview cannot install or save skills.</div>}
    {error && <div className="skills-error" role="alert">{error}</div>}
    {notice && <div className="skills-notice" role="status">{notice}</div>}
    {loading ? <div className="skills-empty">Loading skill catalogue…</div> : <div className="skills-grid">
      {skills.map(skill => <article className="skill-card" key={skill.id}>
        <div className="skill-card-top">
          <div className="skill-icon">{ICONS[skill.id] || "✦"}</div>
          <span className={skill.enabled ? "skill-state enabled" : skill.installed ? "skill-state installed" : "skill-state"}>{skill.enabled ? "ENABLED" : skill.installed ? "INSTALLED" : "AVAILABLE"}</span>
        </div>
        <h3>{skill.name}</h3>
        <p className="skill-description">{skill.description}</p>
        <div className="skill-permissions"><strong>PERMISSIONS WHEN IMPLEMENTED</strong>
          {skill.permissions.map(permission => <span key={permission}>• {permission}</span>)}
        </div>
        <div className="skill-actions">
          {!skill.installed ? <button className="primary-button" disabled={!desktop || !!busyId} onClick={() => void runAction(skill, "install")}>{busyId === skill.id ? "Please wait…" : "Install skill"}</button> : <>
            <button className={skill.enabled ? "secondary-button" : "primary-button"} disabled={!desktop || !!busyId} onClick={() => void runAction(skill, skill.enabled ? "disable" : "enable")}>{skill.enabled ? "Disable" : "Enable"}</button>
            <button className="skill-remove" disabled={!desktop || !!busyId} onClick={() => void runAction(skill, "uninstall")}>Uninstall</button>
          </>}
        </div>
      </article>)}
    </div>}
    <div className="skills-safety"><strong>Early access notice</strong><p>This first Skills Store build installs and remembers skill packages and their enabled state. The actual research, PDF, file and coding tools are not connected yet. No downloaded scripts are executed. We'll implement and test each skill separately before giving it access to your files or network.</p></div>
  </section>;
}
