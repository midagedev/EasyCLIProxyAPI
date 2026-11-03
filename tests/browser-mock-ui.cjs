const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const assert = require('node:assert/strict');
const path = require('node:path');

(async () => {
  const { createServer } = await import('vite');
  const react = (await import('@vitejs/plugin-react')).default;
  const server = await createServer({ configFile: false, root: path.resolve(__dirname, '..'), plugins: [react()],
    logLevel: 'error', server: { host: '127.0.0.1', port: 0, watch: null } });
  let browser;
  try {
    await server.listen();
    const base = `http://127.0.0.1:${server.httpServer.address().port}`;
    browser = await chromium.launch({ channel: process.platform === 'win32' ? 'msedge' : undefined, headless: true, args: ['--no-proxy-server'] });
    const page = await browser.newPage({ viewport: { width: 1505, height: 1000 } });
    page.setDefaultTimeout(30000);
    page.setDefaultNavigationTimeout(120000);
    const errors = [], externalRequests = [];
    page.on('pageerror', error => errors.push(String(error)));
    await page.route('**/*', route => {
      if (route.request().url().startsWith(`${base}/`)) return route.continue();
      externalRequests.push(route.request().url());
      return route.abort();
    });
    await page.addInitScript(() => localStorage.setItem('easy-cli-proxy-api.locale', 'en'));
    await page.goto(`${base}/?mock=running&mockDelay=5`, { waitUntil: 'domcontentloaded' });
    await page.locator('.app-shell').waitFor();
    await page.locator('.nav-section button').nth(2).click();
    await page.getByRole('tab', { name: 'Authentication Files', exact: true }).click();
    await page.locator('.auth-file-card').first().waitFor();
    assert.equal(await page.locator('.auth-file-card').count(), 10);
    await page.locator('.auth-list-pagination select').selectOption('20');
    await page.waitForFunction(() => document.querySelectorAll('.auth-file-card').length > 10);
    await page.locator('.auth-files-heading-actions').getByRole('button', { name: 'Refresh quota', exact: true }).click();
    const card = name => page.locator('.auth-file-card').filter({ has: page.locator(`.auth-card-filename[title="${name}"]`) });
    for (const name of ['codex-personal.json', 'claude-team.json', 'kimi-personal.json', 'devin-team.json', 'xai-subscription.json', 'antigravity-team-with-a-long-account-name.json']) {
      await card(name).locator('[role="progressbar"]').first().waitFor();
    }
    assert.equal(await card('codex-disabled.json').getByRole('switch').getAttribute('aria-checked'), 'false');
    await card('codex-disabled.json').getByRole('switch').click();
    await page.waitForFunction(() => document.querySelector('.auth-card-filename[title="codex-disabled.json"]')?.closest('.auth-file-card')?.querySelector('[role="switch"]')?.getAttribute('aria-checked') === 'true');
    const cooling = card('codex-exhausted.json');
    await cooling.getByRole('button', { name: 'Clear cooldown', exact: true }).click();
    await page.getByRole('alertdialog').getByRole('button', { name: 'Clear cooldown', exact: true }).click();
    await cooling.locator('.auth-file-health-compact').waitFor({ state: 'hidden' });
    await page.screenshot({ path: 'bin-work/browser-mock-credentials.png', fullPage: true });
    await page.evaluate(() => {
      localStorage.setItem('cpa-gui.agent-selected-client.v1', 'codex');
      window.dispatchEvent(new CustomEvent('app:navigate', { detail: 'agents' }));
    });
    await page.locator('#agent-subpage-tab-core').click();
    const openCatalog = async () => {
      await page.getByRole('button', { name: 'Model list settings', exact: true }).click();
      await page.locator('.codex-catalog-models [role="option"]').first().waitFor();
    };
    await openCatalog();
    const dialog = page.getByRole('dialog', { name: 'Codex model catalog', exact: true });
    const options = dialog.locator('.codex-catalog-models [role="option"]');
    const priority = dialog.getByRole('spinbutton', { name: 'Display priority', exact: true });
    const save = dialog.getByRole('button', { name: 'Save', exact: true });
    const saveAndClose = async () => {
      await save.click();
      await page.waitForFunction(() => {
        const footer = document.querySelector('.codex-catalog-footer');
        return footer && [...footer.querySelectorAll('button')].some(button => button.textContent.trim() === 'Save' && button.disabled);
      });
      await dialog.getByRole('button', { name: 'Close', exact: true }).click();
      await dialog.waitFor({ state: 'hidden' });
    };
    assert.match(await options.first().innerText(), /gpt-5.2-codex/);
    await options.filter({ hasText: 'gpt-5.1-codex' }).click();
    await priority.fill('');
    await save.click();
    await dialog.getByText('Display priority must be an integer between -9007199254740991 and 9007199254740991.', { exact: true }).waitFor();
    assert.equal(await dialog.count(), 1);
    await priority.fill('-100');
    assert.match(await options.first().innerText(), /gpt-5.1-codex/);
    assert.equal(await options.first().getAttribute('aria-selected'), 'true');
    await dialog.screenshot({ path: '/tmp/ecpa_issue-225-codex-model-order-1102_ui_changed.png' });
    await saveAndClose();
    await openCatalog();
    assert.match(await options.first().innerText(), /gpt-5.1-codex/);
    assert.equal(await priority.inputValue(), '-100');
    await dialog.getByRole('button', { name: 'Use defaults', exact: true }).click();
    await page.waitForFunction(() => document.querySelector('.codex-catalog-models [role="option"]')?.textContent.includes('gpt-5.2-codex'));
    assert.equal(await priority.inputValue(), '100');
    await saveAndClose();
    await openCatalog();
    assert.match(await options.first().innerText(), /gpt-5.2-codex/);
    assert.equal(await priority.inputValue(), '0');
    assert.equal(await save.isDisabled(), true);
    await dialog.screenshot({ path: '/tmp/ecpa_issue-225-codex-model-order-1102_ui_defaults.png' });
    await dialog.getByRole('button', { name: 'Close', exact: true }).click();
    await page.locator('.sidebar-language-trigger').click();
    await page.locator('#sidebar-language-list [role="option"]').filter({ has: page.locator('[lang="zh-CN"]') }).click();
    await page.getByRole('button', { name: '模型列表设置', exact: true }).click();
    const chineseDialog = page.getByRole('dialog', { name: 'Codex 模型列表', exact: true });
    await chineseDialog.locator('[role="option"]').filter({ hasText: 'gpt-5.1-codex' }).click();
    await chineseDialog.getByRole('spinbutton', { name: '显示优先级', exact: true }).fill('-100');
    assert.match(await chineseDialog.locator('[role="option"]').first().innerText(), /gpt-5.1-codex/);
    await chineseDialog.screenshot({ path: '/tmp/ecpa_issue-225-codex-model-order-1102_ui_zh_changed.png' });
    assert.deepEqual(errors, []);
    assert.deepEqual(externalRequests, []);
    console.log('PASS: real app browser mock, six provider quotas, pagination, credential toggle and cooldown reset without external requests.');
    console.log('PASS: Codex priority validation, live ordering, selection, save/reopen and reset/reopen.');
    console.log('Screenshots: /tmp/ecpa_issue-225-codex-model-order-1102_ui_changed.png, /tmp/ecpa_issue-225-codex-model-order-1102_ui_defaults.png');
    console.log('Chinese screenshot: /tmp/ecpa_issue-225-codex-model-order-1102_ui_zh_changed.png');
  } finally { await browser?.close(); await server.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
