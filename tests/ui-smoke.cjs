// Renderer integration tests with an explicit mocked Tauri boundary, not Windows acceptance.
const assert = require('node:assert/strict');
const { chromium } = require('playwright');
(async () => {
  const path = require('node:path');
  const { spawn } = require('node:child_process');
  const server = spawn(process.execPath, [path.join(__dirname, '../node_modules/vite/bin/vite.js'), '--host', '127.0.0.1', '--port', '1430'], { cwd: path.join(__dirname, '..'), stdio: 'ignore' });
  let browser;
  try {
  for (let attempt = 0; attempt < 50; attempt++) {
    try { const r = await fetch('http://127.0.0.1:1430'); if (r.ok) break; } catch {}
    await new Promise(resolve => setTimeout(resolve, 200));
  }
  browser = await chromium.launch(process.platform === 'win32' ? { channel: 'msedge', headless: true } : { executablePath: process.env.CHROMIUM_PATH || '/usr/bin/chromium', headless: true, args: ['--no-sandbox'] });
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  const errors = [];
  page.on('pageerror', error => errors.push(String(error)));
  await page.goto('http://127.0.0.1:1430');
  assert.equal(await page.getByText('Desktop runtime required.', { exact: false }).count(), 1);
  assert.equal(await page.getByRole('button', { name: 'Connect Ollama' }).isDisabled(), true);
  for (const name of ['Preferences & memory', 'Action history', 'Availability', 'Conversation']) {
    await page.getByRole('button', { name, exact: true }).click();
  }

  await page.addInitScript(() => {
    window.calls = [];
    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        window.calls.push({ command, args });
        const r = args?.request;
        if (command === 'stop_operation') return;
        if (r?.operation === 'models') return { models: [{ name: 'test-local' }] };
        if (r?.operation === 'chat') {
          if (r.messages.at(-1).role === 'tool') return { message: { role: 'assistant', content: 'Observed file listing.' } };
          return { message: { role: 'assistant', content: 'I propose listing the workspace.', tool_calls: [{ function: { name: 'list_files', arguments: {} } }] } };
        }
        if (r?.operation === 'execute') return [{ name: 'note.txt', directory: false }];
        throw new Error('Unexpected test invocation: ' + command);
      }
    };
  });
  await page.reload();
  await page.getByRole('button', { name: 'Connect Ollama' }).click();
  await page.getByLabel('Propose local tools').check();
  await page.getByLabel('Message to Friday').fill('List workspace files');
  await page.getByRole('button', { name: 'Send', exact: true }).click();
  await page.getByRole('region', { name: 'Tool approval' }).waitFor();
  assert.equal(await page.evaluate(() => window.calls.filter(c => c.args?.request?.operation === 'execute').length), 0);
  assert.equal(await page.getByRole('button', { name: 'Send', exact: true }).isDisabled(), true);
  await page.getByRole('button', { name: 'Approve this action' }).click();
  await page.getByText('Observed file listing.').waitFor();
  assert.equal(await page.evaluate(() => window.calls.filter(c => c.args?.request?.operation === 'execute').length), 1);
  await page.getByLabel('Message to Friday').fill('List workspace again');
  await page.getByRole('button', { name: 'Send', exact: true }).click();
  await page.getByRole('region', { name: 'Tool approval' }).waitFor();
  await page.getByRole('button', { name: 'Decline', exact: true }).click();
  await page.getByText('Observed file listing.').last().waitFor();
  assert.equal(await page.evaluate(() => window.calls.filter(c => c.args?.request?.operation === 'execute').length), 1);

  assert.deepEqual(errors, []);
  console.log('PASS: browser runtime gate, navigation, tool approval, rejection, continuation, no renderer errors');
  } finally { if (browser) await browser.close(); server.kill(); }
})().catch(error => { console.error(error); process.exit(1); });
