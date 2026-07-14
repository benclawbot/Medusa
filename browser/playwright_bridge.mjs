#!/usr/bin/env node
// Minimal Playwright bridge for medusa-browserd.
//
// Reads JSON requests from stdin (one per line) and writes JSON responses
// to stdout (one per line). The Rust sidecar owns control plane concerns
// (URL validation, ping, close); this script owns the Playwright API.

import { chromium } from 'playwright';

let browser = null;
let context = null;
let page = null;
let nextRefId = 1;
const refs = new Map();

async function ensurePage() {
  if (!browser) {
    browser = await chromium.launch();
    context = await browser.newContext();
    page = await context.newPage();
  }
  return page;
}

function snapshotFromElement(el, depth = 0) {
  const tag = el.tagName().toLowerCase();
  const role = el.getAttribute('role') ?? tag;
  const name =
    el.getAttribute('aria-label') ??
    el.textContent()?.trim().slice(0, 80) ??
    '';
  const id = el.getAttribute('data-medusa-ref');
  let refId = null;
  if (id) {
    refId = Number.parseInt(id, 10);
  } else {
    refId = nextRefId++;
    el.evaluate((node, value) => node.setAttribute('data-medusa-ref', String(value)), refId);
  }
  refs.set(refId, el);
  const selector = `[data-medusa-ref="${refId}"]`;
  const children = Array.from(el.children()).map((child) => snapshotFromElement(child, depth + 1));
  return { refId, role, name, selector, children };
}

async function snapshot() {
  const p = await ensurePage();
  refs.clear();
  const body = await p.$('body');
  if (!body) return { text: '', refs: [] };
  const tree = snapshotFromElement(body);
  const text = await p.evaluate(() => document.body.innerText);
  const flat = [];
  const flatten = (node) => {
    flat.push({ id: node.refId, role: node.role, name: node.name, selector: node.selector });
    node.children.forEach(flatten);
  };
  flatten(tree);
  return { text, refs: flat };
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
  return {
    kind: 'screenshot',
    format: 'png',
    bytes_base64: buf.toString('base64'),
  };
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
      JSON.stringify({
        kind: 'error',
        code: 'invalid_request',
        message: e.message,
      }) + '\n',
    );
    return;
  }
  const handler = handlers[req.method];
  if (!handler) {
    process.stdout.write(
      JSON.stringify({
        kind: 'error',
        code: 'unknown_method',
        message: `unknown method: ${req.method}`,
      }) + '\n',
    );
    return;
  }
  try {
    const response = await handler(req);
    process.stdout.write(JSON.stringify(response) + '\n');
    if (req.method === 'close') {
      process.exit(0);
    }
  } catch (e) {
    process.stdout.write(
      JSON.stringify({
        kind: 'error',
        code: 'bridge_failure',
        message: e.message ?? String(e),
      }) + '\n',
    );
  }
}