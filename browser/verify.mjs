import { chromium } from 'playwright';
import fs from 'node:fs/promises';
import path from 'node:path';
import process from 'node:process';

function argument(name) {
  const index = process.argv.indexOf(name);
  if (index < 0 || index + 1 >= process.argv.length) throw new Error(`missing ${name}`);
  return process.argv[index + 1];
}

export async function verify({ url, output, expectedText }) {
  await fs.mkdir(output, { recursive: true });
  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ viewport: { width: 1280, height: 720 } });
  await context.tracing.start({ screenshots: true, snapshots: true, sources: true });
  const page = await context.newPage();
  const consoleErrors = [];
  const failedRequests = [];
  page.on('console', message => { if (message.type() === 'error') consoleErrors.push(message.text()); });
  page.on('requestfailed', request => failedRequests.push(`${request.method()} ${request.url()}: ${request.failure()?.errorText ?? 'unknown'}`));
  const response = await page.goto(url, { waitUntil: 'networkidle' });
  const textVisible = await page.getByText(expectedText, { exact: true }).isVisible();
  const screenshot = path.resolve(output, 'screenshot.png');
  const trace = path.resolve(output, 'trace.zip');
  await page.screenshot({ path: screenshot, fullPage: true });
  const accessibilitySnapshot = await page.locator('body').ariaSnapshot();
  await context.tracing.stop({ path: trace });
  const evidence = {
    url,
    screenshot,
    accessibility_snapshot: accessibilitySnapshot,
    console_errors: consoleErrors,
    failed_requests: failedRequests,
    assertions: {
      http_ok: Boolean(response?.ok()),
      expected_text_visible: textVisible,
      title_correct: (await page.title()) === 'Medusa Phase 6'
    },
    viewport: '1280x720',
    browser_version: browser.version(),
    trace
  };
  await browser.close();
  await fs.writeFile(path.join(output, 'evidence.json'), JSON.stringify(evidence, null, 2));
  process.stdout.write(JSON.stringify(evidence));
  return evidence;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  verify({
    url: argument('--url'),
    output: argument('--output'),
    expectedText: argument('--expected-text')
  }).catch(error => { console.error(error); process.exit(1); });
}
