import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';

const html = await readFile(new URL('./interactive.html', import.meta.url));
const largeDom = `<!doctype html><html><body>${'<div>node</div>'.repeat(4100)}</body></html>`;
const largeText = `<!doctype html><html><body>${'x'.repeat(1024 * 1024 + 1024)}</body></html>`;
const hugeScreenshot = '<!doctype html><html><body style="margin:0;width:5000px;height:5000px">huge screenshot fixture</body></html>';
let hangOnceRequests = 0;

const fixture = createServer((request, response) => {
  const requested = new URL(request.url ?? '/', 'http://127.0.0.1:4173');
  console.log(`FIXTURE_REQUEST ${request.method ?? 'GET'} ${request.url ?? ''} -> ${requested.pathname}`);
  if (requested.pathname === '/interactive.html' || requested.pathname === '/') {
    response.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
    response.end(html);
    return;
  }
  if (requested.pathname === '/hang-once.html') {
    hangOnceRequests += 1;
    if (hangOnceRequests === 1) {
      console.log('HANG_ONCE active');
      return;
    }
    response.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
    response.end('<!doctype html><html><body>hang-once recovered</body></html>');
    return;
  }
  if (requested.pathname === '/large-dom.html') {
    response.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
    response.end(largeDom);
    return;
  }
  if (requested.pathname === '/large-text.html') {
    response.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
    response.end(largeText);
    return;
  }
  if (requested.pathname === '/huge-screenshot.html') {
    response.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
    response.end(hugeScreenshot);
    return;
  }
  response.writeHead(404, { 'content-type': 'text/plain; charset=utf-8' });
  response.end('not found');
});

const trap = createServer((request, response) => {
  console.log(`TRAP_HIT ${request.method ?? 'GET'} ${request.url ?? ''}`);
  response.writeHead(204);
  response.end();
});

fixture.listen(4173, '127.0.0.1', () => {
  console.log('browser fixture listening on http://127.0.0.1:4173');
});
trap.listen(4174, '127.0.0.1', () => {
  console.log('browser trap listening on http://127.0.0.1:4174');
});

let closing = false;
function close() {
  if (closing) return;
  closing = true;
  let remaining = 2;
  const done = () => {
    remaining -= 1;
    if (remaining === 0) process.exit(0);
  };
  fixture.close(done);
  trap.close(done);
}

for (const signal of ['SIGINT', 'SIGTERM']) {
  process.on(signal, close);
}
