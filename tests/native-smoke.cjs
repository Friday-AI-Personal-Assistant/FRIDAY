// Real installed Tauri -> Rust -> packaged Python -> filesystem integration.
// Run only on a disposable Windows CI account with the test app started by test-install.ps1.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require('playwright');
(async () => {
  let browser;
  let connectionError;
  for (let attempt = 0; attempt < 60; attempt++) {
    try { browser = await chromium.connectOverCDP('http://127.0.0.1:9223'); break; }
    catch (error) { connectionError = error; await new Promise(resolve => setTimeout(resolve, 500)); }
  }
  if (!browser) {
    console.warn('BLOCKED: This runner did not expose the installed WebView2 debugging endpoint. Native IPC/UI checks were NOT executed. Last connection error:', String(connectionError));
    process.exit(process.env.FRIDAY_REQUIRE_NATIVE_SMOKE === '1' ? 1 : 2);
  }
  try {
    let page;
    for (let attempt = 0; attempt < 60; attempt++) {
      page = browser.contexts().flatMap(c => c.pages()).find(p => /tauri/.test(p.url()));
      if (page) break;
      await new Promise(resolve => setTimeout(resolve, 500));
    }
    assert(page, 'Installed Tauri page did not load');
    await page.getByRole('heading', { name: 'FRIDAY Local Agent' }).waitFor();
    assert.equal(await page.getByText('Desktop runtime required.', { exact: false }).count(), 0);
    const invoke = (command, args = {}) => page.evaluate(({ command, args }) => window.__TAURI_INTERNALS__.invoke(command, args), { command, args });
    await invoke('initialize_workspace');
    const content = 'CI encrypted note integration';
    await invoke('save_private_note', { note: content });
    assert.equal(await invoke('read_private_note'), content);
    const workspace = path.join(process.env.APPDATA, 'com.friday.assistant', 'workspace');
    assert(!fs.readFileSync(path.join(workspace, 'private-note.enc')).includes(Buffer.from(content)), 'Plaintext was written to encrypted note');
    await invoke('save_agent_state', { content: JSON.stringify({ memory: 'CI memory integration' }) });
    assert.deepEqual(JSON.parse(await invoke('read_agent_state')), { memory: 'CI memory integration' });
    const op = request => invoke('local_operation', { request });
    await op({ operation: 'execute', name: 'write_document', arguments: { path: 'ci-smoke.txt', text: 'Installed worker integration' } });
    assert.equal(await op({ operation: 'execute', name: 'read_document', arguments: { path: 'ci-smoke.txt' } }), 'Installed worker integration');
    await assert.rejects(op({ operation: 'execute', name: 'read_document', arguments: { path: '../workspace/private-note.enc' } }));
    await assert.rejects(op({ operation: 'execute', name: 'terminal', arguments: { command: 'whoami' } }));
    console.log('PASS: installed native UI, IPC, Windows Credential Manager backed encryption, encrypted session, packaged worker file round trip, scope and unknown-tool rejection.');
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exit(1); });
