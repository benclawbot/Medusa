import fs from 'node:fs/promises';
import http from 'node:http';
import path from 'node:path';
import process from 'node:process';
import { verify } from './verify.mjs';

const fixture = path.resolve('fixture');
await fs.copyFile(path.join(fixture, 'after.html'), path.join(fixture, 'index.html'));
const server = http.createServer(async (request, response) => {
  if (request.url !== '/') { response.writeHead(404); response.end(); return; }
  response.setHeader('content-type', 'text/html; charset=utf-8');
  response.end(await fs.readFile(path.join(fixture, 'index.html')));
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const address = server.address();
try {
  const evidence = await verify({
    url: `http://127.0.0.1:${address.port}/`,
    output: path.resolve('artifacts'),
    expectedText: 'Medusa Phase 6 verified'
  });
  if (!Object.values(evidence.assertions).every(Boolean)) throw new Error('browser assertion failed');
  if (evidence.console_errors.length || evidence.failed_requests.length) throw new Error('browser errors detected');
  await fs.access(evidence.screenshot);
  await fs.access(evidence.trace);
  console.log('\nvisual-verification-ok');
} finally {
  server.close();
}
