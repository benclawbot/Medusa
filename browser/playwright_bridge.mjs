#!/usr/bin/env node
// Bounded Playwright bridge for medusa-browserd.

import { chromium } from 'playwright';

const MAX_REQUEST_FRAME_BYTES = 64 * 1024;
const MAX_RESPONSE_FRAME_BYTES = 12 * 1024 * 1024;
const MAX_DOM_REFS = 4096;
const MAX_SNAPSHOT_TEXT_BYTES = 1024 * 1024;
const MAX_EVALUATION_BYTES = 1024 * 1024;
const MAX_SCREENSHOT_BYTES = 8 * 1024 * 1024;
const MAX_SCREENSHOT_PIXELS = 16 * 1024 * 1024;

let browser = null;
let context = null;
let page = null;
let nextRefId = 1;

async function ensurePage() {
  if (!browser) {
    const proxyServer = process.env.MEDUSA_BROWSER_PROXY;
    if (!proxyServer) throw new Error('MEDUSA_BROWSER_PROXY is required');
    browser = await chromium.launch({ proxy: { server: proxyServer, bypass: '<-loopback>' } });
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

async function snapshot() {
  const p = await ensurePage();
  const result = await p.evaluate(
    ({ startRef, maxRefs, maxTextBytes }) => {
      const body = document.body;
      if (!body) return { ok: true, nextRef: startRef, text: '', refs: [] };
      const refs = [];
      let nextRef = startRef;
      const directName = (element) => {
        for (const attribute of ['aria-label', 'alt', 'title', 'value']) {
          const value = element.getAttribute(attribute);
          if (value) return value.trim().slice(0, 80);
        }
        let value = '';
        for (const child of element.childNodes) {
          if (child.nodeType !== Node.TEXT_NODE) continue;
          value += ` ${child.nodeValue ?? ''}`;
          if (value.length >= 80) break;
        }
        return value.trim().replace(/\s+/g, ' ').slice(0, 80);
      };
      const walker = document.createTreeWalker(body, NodeFilter.SHOW_ELEMENT);
      let element = body;
      while (element) {
        if (refs.length >= maxRefs) {
          return { ok: false, code: 'dom_too_large', message: `DOM snapshot exceeds ${maxRefs} element references` };
        }
        let refId = Number.parseInt(element.getAttribute('data-medusa-ref') ?? '', 10);
        if (!Number.isSafeInteger(refId) || refId <= 0) {
          refId = nextRef++;
          element.setAttribute('data-medusa-ref', String(refId));
        }
        const tag = element.tagName.toLowerCase();
        refs.push({ id: refId, role: element.getAttribute('role') ?? tag, name: directName(element), selector: `[data-medusa-ref="${refId}"]` });
        element = walker.nextNode();
      }
      const encoder = new TextEncoder();
      const textParts = [];
      let textBytes = 0;
      const textWalker = document.createTreeWalker(body, NodeFilter.SHOW_TEXT);
      let textNode = textWalker.nextNode();
      while (textNode) {
        const value = (textNode.nodeValue ?? '').replace(/\s+/g, ' ').trim();
        if (value) {
          const piece = `${textParts.length === 0 ? '' : ' '}${value}`;
          const pieceBytes = encoder.encode(piece).length;
          if (textBytes + pieceBytes > maxTextBytes) {
            return { ok: false, code: 'snapshot_text_too_large', message: `snapshot text exceeds ${maxTextBytes} UTF-8 bytes` };
          }
          textParts.push(piece);
          textBytes += pieceBytes;
        }
        textNode = textWalker.nextNode();
      }
      return { ok: true, nextRef, text: textParts.join(''), refs };
    },
    { startRef: nextRefId, maxRefs: MAX_DOM_REFS, maxTextBytes: MAX_SNAPSHOT_TEXT_BYTES },
  );
  if (!result.ok) return { kind: 'error', code: result.code, message: result.message };
  nextRefId = result.nextRef;
  return { kind: 'snapshot', text: result.text, refs: result.refs };
}

async function click(request) {
  const p = await ensurePage();
  if (request.ref_id != null) { await p.click(`[data-medusa-ref="${request.ref_id}"]`); return { kind: 'ok' }; }
  if (request.selector) { await p.click(request.selector); return { kind: 'ok' }; }
  return { kind: 'error', code: 'missing_target', message: 'click requires ref_id or selector' };
}

async function fill(request) {
  const p = await ensurePage();
  if (request.ref_id != null) { await p.fill(`[data-medusa-ref="${request.ref_id}"]`, request.value); return { kind: 'ok' }; }
  if (request.selector) { await p.fill(request.selector, request.value); return { kind: 'ok' }; }
  return { kind: 'error', code: 'missing_target', message: 'fill requires ref_id or selector' };
}

async function press(request) {
  const p = await ensurePage();
  await p.keyboard.press(request.key);
  return { kind: 'ok' };
}

async function screenshot(request) {
  const p = await ensurePage();
  const dimensions = await p.evaluate((fullPage) => {
    if (!fullPage) return { width: innerWidth, height: innerHeight };
    const root = document.documentElement;
    return { width: Math.max(root.scrollWidth, root.clientWidth), height: Math.max(root.scrollHeight, root.clientHeight) };
  }, !!request.full_page);
  const pixels = Math.max(0, dimensions.width) * Math.max(0, dimensions.height);
  if (!Number.isFinite(pixels) || pixels > MAX_SCREENSHOT_PIXELS) {
    return { kind: 'error', code: 'screenshot_dimensions_too_large', message: `screenshot exceeds ${MAX_SCREENSHOT_PIXELS} pixels` };
  }
  const buf = await p.screenshot({ fullPage: !!request.full_page });
  if (buf.length > MAX_SCREENSHOT_BYTES) {
    return { kind: 'error', code: 'screenshot_too_large', message: `screenshot exceeds ${MAX_SCREENSHOT_BYTES} bytes` };
  }
  return { kind: 'screenshot', format: 'png', bytes_base64: buf.toString('base64') };
}

async function evaluate(request) {
  const p = await ensurePage();
  const bounded = await p.evaluate(
    ({ expression, maxBytes }) => {
      const value = (0, eval)(expression);
      let serialized;
      try { serialized = JSON.stringify(value); } catch (error) {
        return { ok: false, code: 'evaluation_unserializable', message: String(error?.message ?? error) };
      }
      if (serialized === undefined) serialized = 'null';
      const size = new TextEncoder().encode(serialized).length;
      if (size > maxBytes) return { ok: false, code: 'evaluation_too_large', message: `evaluation result exceeds ${maxBytes} UTF-8 bytes` };
      return { ok: true, value: JSON.parse(serialized) };
    },
    { expression: request.expression, maxBytes: MAX_EVALUATION_BYTES },
  );
  if (!bounded.ok) return { kind: 'error', code: bounded.code, message: bounded.message };
  return { kind: 'evaluate', value: bounded.value };
}

async function tabs() {
  if (!browser) return { kind: 'tabs', tabs: [] };
  const pages = browser.contexts()[0]?.pages() ?? [];
  return { kind: 'tabs', tabs: pages.map((p, idx) => ({ id: idx, url: p.url(), title: p.url() })) };
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
    return { kind: 'navigate', final_url: p.url(), status: resp ? resp.status() : 0 };
  },
  snapshot,
  click,
  fill,
  press,
  screenshot,
  evaluate,
  tabs,
  close: async () => { await close(); return { kind: 'ok' }; },
};

async function emitResponse(requestId, response) {
  let payload = JSON.stringify({ request_id: requestId, ...response });
  if (Buffer.byteLength(payload, 'utf8') + 1 > MAX_RESPONSE_FRAME_BYTES) {
    payload = JSON.stringify({ request_id: requestId, kind: 'error', code: 'response_too_large', message: `browser response exceeds ${MAX_RESPONSE_FRAME_BYTES} bytes` });
  }
  await new Promise((resolve, reject) => {
    process.stdout.write(`${payload}\n`, (error) => (error ? reject(error) : resolve()));
  });
}

async function handleLine(line) {
  if (!line.trim()) return;
  if (Buffer.byteLength(line, 'utf8') + 1 > MAX_REQUEST_FRAME_BYTES) {
    await emitResponse(0, { kind: 'error', code: 'request_too_large', message: `browser request exceeds ${MAX_REQUEST_FRAME_BYTES} bytes` });
    return;
  }
  let req;
  try { req = JSON.parse(line); } catch (error) {
    await emitResponse(0, { kind: 'error', code: 'invalid_request', message: error.message });
    return;
  }
  const requestId = req.request_id;
  if (!Number.isSafeInteger(requestId) || requestId <= 0) {
    await emitResponse(0, { kind: 'error', code: 'invalid_request_id', message: 'browser request_id must be a positive integer' });
    return;
  }
  const handler = handlers[req.method];
  if (!handler) {
    await emitResponse(requestId, { kind: 'error', code: 'unknown_method', message: `unknown method: ${req.method}` });
    return;
  }
  try {
    const response = await handler(req);
    await emitResponse(requestId, response);
    if (req.method === 'close') process.exit(0);
  } catch (error) {
    await emitResponse(requestId, { kind: 'error', code: 'bridge_failure', message: error?.message ?? String(error) });
  }
}

let inputBuffer = '';
let requestChain = Promise.resolve();
process.stdin.setEncoding('utf-8');
process.stdin.on('data', (chunk) => {
  inputBuffer += chunk;
  let newline;
  while ((newline = inputBuffer.indexOf('\n')) !== -1) {
    const line = inputBuffer.slice(0, newline);
    inputBuffer = inputBuffer.slice(newline + 1);
    requestChain = requestChain.then(() => handleLine(line));
  }
  if (Buffer.byteLength(inputBuffer, 'utf8') > MAX_REQUEST_FRAME_BYTES) {
    inputBuffer = '';
    requestChain = requestChain.then(() => emitResponse(0, { kind: 'error', code: 'request_too_large', message: `unterminated browser request exceeds ${MAX_REQUEST_FRAME_BYTES} bytes` }));
  }
});

const expectedParentPid = Number.parseInt(process.env.MEDUSA_BROWSER_PARENT_PID ?? '', 10);
if (Number.isSafeInteger(expectedParentPid) && expectedParentPid > 0) {
  setInterval(() => {
    if (process.ppid === expectedParentPid) return;
    void close().finally(() => process.exit(0));
  }, 50).unref();
}
