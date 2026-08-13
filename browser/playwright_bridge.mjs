#!/usr/bin/env node
// Minimal Playwright bridge for medusa-browserd.

import { chromium } from 'playwright';

let browser = null;
let context = null;
let page = null;
let nextRefId = 1;
const refs = new Map();

async function ensurePage() {
  if (!browser) {
    const proxyServer = process.env.MEDUSA_BROWSER_PROXY;
    if (!proxyServer) throw new Error('MEDUSA_BROWSER_PROXY is required');
    // Chromium implicitly bypasses proxies for localhost/link-local destinations.
    // The special <-loopback> subtraction rule keeps those requests inside
    // medusa-browserd, where the exact verification-origin policy is enforced.
    browser = await chromium.launch({
      proxy: { server: proxyServer, bypass: '<-loopback>' },
    });
    context = await browser.newContext({ serviceWorkers: 'block' });
    await context.addInitScript(() => {
      globalThis.__MEDUSA_CONSOLE_ERRORS__ = [];
      const originalError = console.error.bind(console);
      console.error = (...args) => {
        globalThis.__MEDUSA_CONSOLE_ERRORS__.push(args.map(String).join(' '));
        originalError(...args);
      };
      addEventListener('error', (event) => {
        globalThis.__MEDUSA_CONSOLE_ERRORS__.push(String(event.error?.stack ?? event.message));
      });
      addEventListener('unhandledrejection', (event) => {
        globalThis.__MEDUSA_CONSOLE_ERRORS__.push(String(event.reason?.stack ?? event.reason));
      });
    });
    page = await context.newPage();
  }
  return page;
}

async function snapshotFromElement(el, depth = 0) {
  const tag = (await el.evaluate((node) => node.tagName)).toLowerCase();
  const role = (await el.getAttribute('role')) ?? tag;
  const textContent = await el.textContent();
  const name = (await el.getAttribute('aria-label')) ?? textContent?.trim().slice(0, 80) ?? '';
  const id = await el.getAttribute('data-medusa-ref');
  let refId = null;
  if (id) {
    refId = Number.parseInt(id, 10);
  } else {
    refId = nextRefId++;
    await el.evaluate((node, value) => node.setAttribute('data-medusa-ref', String(value)), refId);
  }
  refs.set(refId, el);
  const selector = `[data-medusa-ref="${refId}"]`;
  const children = [];
  for (const child of await el.locator(':scope > *').all()) {
    children.push(await snapshotFromElement(child, depth + 1));
  }
  return { refId, role, name, selector, children };
}

async function snapshot() {
  const p = await ensurePage();
  refs.clear();
  const body = p.locator('body').first();
  if ((await body.count()) === 0) return { kind: 'snapshot', text: '', refs: [] };
  const tree = await snapshotFromElement(body);
  const text = await p.evaluate(() => document.body.innerText);
  const flat = [];
  const flatten = (node) => {
    flat.push({ id: node.refId, role: node.role, name: node.name, selector: node.selector });
    node.children.forEach(flatten);
  };
  flatten(tree);
  return { kind: 'snapshot', text, refs: flat };
}

async function click(request) {
  const p = await ensurePage();
  if (request.ref_id != null) {
    await p.click(`[data-medusa-ref="${request.ref_id}"]`);
    return { kind: 'ok' };
  }
  if (request.selector) {
    await p.click(request.selector);
    return { kind: 'ok' };
  }
  return { kind: 'error', code: 'missing_target', message: 'click requires ref_id or selector' };
}

async function fill(request) {
  const p = await ensurePage();
  if (request.ref_id != null) {
    await p.fill(`[data-medusa-ref="${request.ref_id}"]`, request.value);
    return { kind: 'ok' };
  }
  if (request.selector) {
    await p.fill(request.selector, request.value);
    return { kind: 'ok' };
  }
  return { kind: 'error', code: 'missing_target', message: 'fill requires ref_id or selector' };
}

async function press(request) {
  const p = await ensurePage();
  await p.keyboard.press(request.key);
  return { kind: 'ok' };
}

async function screenshot(request) {
  const p = await ensurePage();
  const buf = await p.screenshot({ fullPage: !!request.full_page });
  return { kind: 'screenshot', format: 'png', bytes_base64: buf.toString('base64') };
}

async function evaluate(request) {
  const p = await ensurePage();
  const value = await p.evaluate(request.expression);
  return { kind: 'evaluate', value };
}

async function tabs() {
  if (!browser) return { kind: 'tabs', tabs: [] };
  const pages = browser.contexts()[0]?.pages() ?? [];
  return {
    kind: 'tabs',
    tabs: pages.map((p, idx) => ({ id: idx, url: p.url(), title: p.url() })),
  };
}

async function close() {
  if (browser) {
    await browser.close();
    browser = null;
    context = null;
    page = null;
  }
}

const handlers = {
  ping: async () => ({ kind: 'ok' }),
  navigate: async (req) => {
    const p = await ensurePage();
    const resp = await p.goto(req.url, { waitUntil: 'domcontentloaded' });
    return {
      kind: 'navigate',
      final_url: p.url(),
      status: resp ? resp.status() : 0,
    };
  },
  snapshot,
  click,
  fill,
  press,
  screenshot,
  evaluate,
  tabs,
  close,
};

let inputBuffer = '';
process.stdin.setEncoding('utf-8');
process.stdin.on('data', (chunk) => {
  inputBuffer += chunk;
  let nl;
  while ((nl = inputBuffer.indexOf('\n')) !== -1) {
    const line = inputBuffer.slice(0, nl);
    inputBuffer = inputBuffer.slice(nl + 1);
    void handleLine(line);
  }
});

async function handleLine(line) {
  if (!line.trim()) return;
  let req;
  try {
    req = JSON.parse(line);
  } catch (e) {
    process.stdout.write(
      JSON.stringify({ kind: 'error', code: 'invalid_request', message: e.message }) + '\n',
    );
    return;
  }
  const handler = handlers[req.method];
  if (!handler) {
    process.stdout.write(
      JSON.stringify({ kind: 'error', code: 'unknown_method', message: `unknown method: ${req.method}` }) + '\n',
    );
    return;
  }
  try {
    const response = await handler(req);
    process.stdout.write(JSON.stringify(response) + '\n');
    if (req.method === 'close') process.exit(0);
  } catch (e) {
    process.stdout.write(
      JSON.stringify({ kind: 'error', code: 'bridge_failure', message: e.message ?? String(e) }) + '\n',
    );
  }
}
