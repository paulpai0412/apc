// Real backend, real Secure cookies, no API interception or fixture responses.
import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { mkdir, writeFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
const { chromium } = await import(pathToFileURL(process.env.APC_PLAYWRIGHT_MODULE).href);
const browser = await chromium.launch({ headless: true, args: ['--host-resolver-rules=MAP localhost 127.0.0.1'] });
const password = randomBytes(24).toString('base64url');
const checks = [];
await mkdir('container-evidence', { recursive: true });
try {
  for (const [label, viewport] of [['desktop', { width: 1440, height: 1000 }], ['mobile', { width: 390, height: 844 }]]) {
    const context = await browser.newContext({ viewport });
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.goto('http://localhost:18082', { waitUntil: 'networkidle' });
    await page.getByLabel('帳號', { exact: true }).fill('admin');
    await page.getByLabel('密碼', { exact: true }).fill(label === 'desktop' ? 'password' : password);
    await page.getByRole('button', { name: '登入', exact: true }).click();
    if (label === 'desktop') {
      await page.getByRole('heading', { name: '首次登入：請設定新密碼' }).waitFor();
      await page.getByLabel('新密碼（至少 8 字元）', { exact: true }).fill(password);
      await page.getByRole('button', { name: '更新密碼', exact: true }).click();
      await page.getByRole('heading', { name: '首次登入：請設定新密碼' }).waitFor({ state: 'hidden' });
      // Fresh capabilities and data after the mandatory account transition.
      await page.reload({ waitUntil: 'networkidle' });
    }
    await page.getByRole('heading', { name: '方案摘要與任務', exact: true }).waitFor();
    await page.getByText('目前缺項', { exact: true }).first().waitFor();
    const me = await page.evaluate(() => fetch('/api/auth/me').then(r => r.json()));
    assert.equal(me.authenticated, true);
    assert.equal(me.force_password_change, false);
    const session = (await context.cookies()).find(cookie => cookie.name.startsWith('authrity_'));
    assert(session?.secure && session.httpOnly);
    if (label === 'mobile') await page.getByRole('button', { name: '開啟功能導覽' }).click();
    assert.equal(await page.getByRole('button', { name: '運行監控', exact: true }).isDisabled(), true);
    if (label === 'mobile') await page.getByRole('button', { name: '關閉功能導覽' }).click();
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
    await page.screenshot({ path: `container-evidence/${label}-actual-hmi.png`, fullPage: true });
    assert.deepEqual(errors, []);
    checks.push({ viewport: label, real_backend: true, login: 'passed', secure_cookie: true,
      isolated_runtime_gate: 'preserved', horizontal_overflow: false, page_errors: errors });
    await context.close();
  }
  await writeFile('container-evidence/browser-smoke.json', JSON.stringify(checks, null, 2));
} finally { await browser.close(); }
