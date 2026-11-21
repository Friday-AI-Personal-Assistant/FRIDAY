import { type FormEvent, useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type PermissionGrant = {
  id: string; capability: string; resource: string; grantedAt: number; expiresAt: number | null;
};
type PermissionAuditEvent = {
  grantId: string; action: string; capability: string; resource: string; occurredAt: number; expiresAt: number | null;
};
type PermissionSnapshot = { grants: PermissionGrant[]; audit: PermissionAuditEvent[] };

function formatDate(timestamp: number | null) {
  return timestamp === null ? "No automatic expiry" : new Date(timestamp * 1000).toLocaleString();
}

function PermissionsCenter({ isDesktopApp }: { isDesktopApp: boolean }) {
  const [snapshot, setSnapshot] = useState<PermissionSnapshot>({ grants: [], audit: [] });
  const [resource, setResource] = useState("html.duckduckgo.com");
  const [expiry, setExpiry] = useState("2592000");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");

  const refresh = useCallback(async () => {
    try {
      const data = await invoke<PermissionSnapshot>("list_permission_grants");
      setSnapshot(data);
      setError("");
    } catch (reason) {
      setError(String(reason));
    }
  }, []);

  useEffect(() => {
    if (isDesktopApp) void refresh();
  }, [isDesktopApp, refresh]);

  function validDomain(value: string) {
    if (value.length > 253 || value.includes("://") || value.includes("/") || value.includes("*") || value.includes(":")) return false;
    return value.split(".").every(label => /^[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?$/.test(label)) && value.includes(".");
  }

  async function grantAccess(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const domain = resource.trim().toLowerCase();
    if (!validDomain(domain)) {
      setError("Enter one exact domain, such as example.com. Wildcards, URLs and paths are not accepted.");
      return;
    }
    setBusy(true); setError(""); setMessage("");
    try {
      const grant = await invoke<PermissionGrant>("grant_permission", {
        capability: "web.research",
        resource: domain,
        expiresInSeconds: expiry === "never" ? null : Number(expiry)
      });
      setMessage("Access granted for " + grant.resource + ". Consequential actions still require their own approval.");
      await refresh();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  }

  async function revoke(grantId: string) {
    setBusy(true); setError(""); setMessage("");
    try {
      const result = await invoke<string>("revoke_permission", { grantId });
      setMessage(result);
      await refresh();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="page compact-page permissions-page">
      <div className="eyebrow"><span className="permissions-dot" /> USER CONTROL CENTRE</div>
      <h1>Your access.<br /><em>Your decision.</em></h1>
      <p className="intro">Review what FRIDAY is allowed to access, grant narrowly scoped Web Research access, and revoke it whenever you choose.</p>
      {!isDesktopApp ? <div className="permissions-browser-notice">
        <strong>Desktop app required</strong>
        <p>This browser preview cannot read or change local permission grants. Open the installed FRIDAY desktop app to manage real permissions.</p>
      </div> : <>
        <div className="permissions-summary">
          <div><span>ACTIVE GRANTS</span><strong>{snapshot.grants.length}</strong></div>
          <div><span>AUDIT EVENTS</span><strong>{snapshot.audit.length}</strong></div>
          <div><span>FEATURES CONNECTED</span><strong>Web Research</strong></div>
        </div>
        <div className="permissions-panel">
          <div className="permissions-panel-heading"><span>01 / REQUEST ACCESS</span><small>Exact domain only</small></div>
          <h2>Allow Web Research to contact a website</h2>
          <p>Web search sends your search query to DuckDuckGo. Reading a result contacts that website. FRIDAY checks each exact domain in its Rust backend, including every redirect destination.</p>
          <form className="permissions-form" onSubmit={grantAccess}>
            <label>Website domain
              <input value={resource} onChange={event => setResource(event.target.value)} placeholder="example.com" maxLength={253} autoComplete="off" spellCheck={false} />
            </label>
            <label>Permission duration
              <select value={expiry} onChange={event => setExpiry(event.target.value)}>
                <option value="86400">1 day</option><option value="604800">7 days</option>
                <option value="2592000">30 days</option><option value="7776000">90 days</option>
                <option value="never">Until I revoke it</option>
              </select>
            </label>
            <button className="permissions-primary" type="submit" disabled={busy}>{busy ? "Please wait…" : "Review & grant access"}</button>
          </form>
          <p className="permissions-disclosure">A native operating-system confirmation appears before FRIDAY saves a grant. Cancel means no access. Search-provider access is separate: grant <code>html.duckduckgo.com</code> if you want to use web search.</p>
        </div>
        <div className="permissions-section-heading"><div><span>02 / CURRENT ACCESS</span><h2>Active permissions</h2></div><button className="permissions-secondary" onClick={() => void refresh()} disabled={busy}>Refresh</button></div>
        {snapshot.grants.length === 0 ? <div className="permissions-empty">No active permissions. FRIDAY will deny Web Research network access until you grant an exact domain.</div> :
          <div className="permissions-grant-list">{snapshot.grants.map(grant => <article className="permissions-grant" key={grant.id}>
            <div className="permissions-grant-copy"><span>{grant.capability}</span><strong>{grant.resource}</strong><small>Granted {formatDate(grant.grantedAt)} · Expires {formatDate(grant.expiresAt)}</small></div>
            <button className="permissions-revoke" disabled={busy} onClick={() => void revoke(grant.id)}>Revoke</button>
          </article>)}</div>}
        <div className="permissions-section-heading"><div><span>03 / ACCOUNTABILITY</span><h2>Recent permission activity</h2></div></div>
        {snapshot.audit.length === 0 ? <div className="permissions-empty">No permission activity has been recorded yet.</div> :
          <div className="permissions-audit-list">{[...snapshot.audit].reverse().map((event, index) => <div className="permissions-audit-row" key={event.grantId + event.action + event.occurredAt + index}>
            <span className={"permissions-event-dot " + event.action} />
            <div><strong>{event.action === "granted" ? "Access granted" : event.action === "revoked" ? "Access revoked" : "Access expired"}</strong><small>{event.resource} · {formatDate(event.occurredAt)}</small></div>
          </div>)}</div>}
      </>}
      {message && <p className="permissions-message" role="status">{message}</p>}
      {error && <p className="permissions-error" role="alert">{error}</p>}
      <div className="permissions-guarantees">
        <strong>FRIDAY's four guarantees</strong>
        <span>Safety · User control · Cost transparency · Reliability</span>
        <p>Persistent grants never replace one-use approval for consequential actions. Installing or enabling a skill never grants it access.</p>
      </div>
    </section>
  );
}
export default PermissionsCenter;
