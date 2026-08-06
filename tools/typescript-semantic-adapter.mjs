import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { pathToFileURL } from 'node:url';
import { createRequire } from 'node:module';

const MAX_RESULTS = 2000;
const IGNORED_PARTS = new Set(['node_modules', '.git', '.medusa', 'target', 'dist', 'build', 'coverage', '.next', '.turbo', 'out', 'generated', 'vendor']);
const SUPPORTED = new Set(['.ts', '.tsx', '.js', '.jsx', '.mts', '.cts', '.mjs', '.cjs']);

const input = JSON.parse(fs.readFileSync(0, 'utf8'));
const repositoryRoot = canonical(input.repository_root);
const workspaceRoot = canonical(input.workspace_root);
assertInside(repositoryRoot, workspaceRoot, 'workspace root');
const packageRoot = input.package_root ? canonical(input.package_root) : workspaceRoot;
assertInside(repositoryRoot, packageRoot, 'package root');

const typescriptPath = findTypeScript(packageRoot, workspaceRoot, repositoryRoot);
if (!typescriptPath) fail('typescript_dependency_unavailable', 'No repository-local or system TypeScript compiler module was found');
const ts = await import(pathToFileURL(typescriptPath).href);
const configPath = input.config_path ? canonical(input.config_path) : null;
if (configPath) assertInside(repositoryRoot, configPath, 'config path');
const project = loadProject(ts, workspaceRoot, configPath);
const service = createService(ts, project.files, project.options);
const workspaceFingerprint = fingerprint(repositoryRoot, project.files);
if (input.expected_workspace_fingerprint && input.expected_workspace_fingerprint !== workspaceFingerprint) {
  fail('stale_workspace', `Workspace fingerprint changed: expected ${input.expected_workspace_fingerprint}, got ${workspaceFingerprint}`);
}

const evidence = {
  adapter: 'typescript-compiler-language-service',
  adapter_version: ts.version,
  repository_root: repositoryRoot,
  workspace_root: workspaceRoot,
  package_root: packageRoot,
  config_path: configPath,
  source_count: project.files.length,
  workspace_fingerprint: workspaceFingerprint,
};

let result;
switch (input.operation) {
  case 'definition': result = positionResults(ts, service.getDefinitionAtPosition.bind(service), input, repositoryRoot); break;
  case 'references': result = positionResults(ts, service.getReferencesAtPosition.bind(service), input, repositoryRoot); break;
  case 'diagnostics': result = diagnostics(ts, service, input, repositoryRoot, project.files); break;
  case 'workspace_symbols': result = workspaceSymbols(ts, service, input.query ?? '', repositoryRoot, project.files); break;
  case 'rename': result = rename(ts, service, input, repositoryRoot); break;
  default: fail('invalid_operation', `Unsupported operation: ${input.operation}`);
}
process.stdout.write(JSON.stringify({ evidence, result }));

function canonical(value) { return fs.realpathSync.native(path.resolve(value)); }
function normalize(value) { return value.split(path.sep).join('/'); }
function inside(root, candidate) { const rel = path.relative(root, candidate); return rel === '' || (!rel.startsWith('..') && !path.isAbsolute(rel)); }
function assertInside(root, candidate, label) { if (!inside(root, candidate)) fail('scope_escape', `${label} is outside repository: ${candidate}`); }
function ignored(file) {
  const parts = normalize(file).split('/');
  const base = path.basename(file);
  return parts.some((part) => IGNORED_PARTS.has(part)) || base.endsWith('.d.ts') || base.endsWith('.min.js');
}
function supported(file) { return SUPPORTED.has(path.extname(file)) && !ignored(file); }
function findTypeScript(...roots) {
  const candidates = [];
  for (const root of roots) candidates.push(path.join(root, 'node_modules', 'typescript', 'lib', 'typescript.js'));
  if (process.env.NODE_PATH) for (const root of process.env.NODE_PATH.split(path.delimiter)) candidates.push(path.join(root, 'typescript', 'lib', 'typescript.js'));
  try { candidates.push(createRequire(import.meta.url).resolve('typescript')); } catch {}
  return candidates.find((candidate) => fs.existsSync(candidate)) ?? null;
}
function loadProject(ts, root, configPath) {
  if (configPath) {
    const read = ts.readConfigFile(configPath, ts.sys.readFile);
    if (read.error) fail('config_error', flatten(ts, read.error.messageText));
    const parsed = ts.parseJsonConfigFileContent(read.config, ts.sys, path.dirname(configPath), undefined, configPath);
    if (parsed.errors.length) fail('config_error', parsed.errors.map((error) => flatten(ts, error.messageText)).join('\n'));
    const files = parsed.fileNames.filter(supported).sort();
    return { files, options: { ...parsed.options, allowJs: true, checkJs: true, noEmit: true } };
  }
  const files = ts.sys.readDirectory(root, [...SUPPORTED], ['**/node_modules/**', '**/.git/**', '**/.medusa/**', '**/target/**', '**/dist/**', '**/build/**', '**/coverage/**', '**/.next/**', '**/.turbo/**', '**/out/**', '**/generated/**', '**/vendor/**']).filter(supported).sort();
  return { files, options: { allowJs: true, checkJs: true, noEmit: true, target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext, moduleResolution: ts.ModuleResolutionKind.Bundler, jsx: ts.JsxEmit.ReactJSX } };
}
function createService(ts, files, options) {
  const versions = new Map(files.map((file) => [file, '1']));
  const host = {
    getScriptFileNames: () => files,
    getScriptVersion: (file) => versions.get(file) ?? '1',
    getScriptSnapshot: (file) => fs.existsSync(file) ? ts.ScriptSnapshot.fromString(fs.readFileSync(file, 'utf8')) : undefined,
    getCurrentDirectory: () => workspaceRoot,
    getCompilationSettings: () => options,
    getDefaultLibFileName: (opts) => ts.getDefaultLibFilePath(opts),
    fileExists: ts.sys.fileExists,
    readFile: ts.sys.readFile,
    readDirectory: ts.sys.readDirectory,
    directoryExists: ts.sys.directoryExists,
    getDirectories: ts.sys.getDirectories,
    realpath: ts.sys.realpath,
    useCaseSensitiveFileNames: () => ts.sys.useCaseSensitiveFileNames,
    getNewLine: () => ts.sys.newLine,
  };
  return ts.createLanguageService(host, ts.createDocumentRegistry());
}
function targetFile(input, root) {
  if (!input.path) fail('invalid_input', 'path is required');
  const file = canonical(path.resolve(root, input.path));
  assertInside(root, file, 'target file');
  if (!supported(file)) fail('unsupported_file', `Unsupported or ignored TypeScript/JavaScript file: ${file}`);
  return file;
}
function offsetFor(ts, service, file, line, character) {
  const source = service.getProgram()?.getSourceFile(file);
  if (!source) fail('file_not_in_project', `File is not part of the selected TypeScript project: ${file}`);
  if (!Number.isInteger(line) || !Number.isInteger(character) || line < 0 || character < 0) fail('invalid_position', 'line and character must be non-negative integers');
  try { return source.getPositionOfLineAndCharacter(line, character); } catch { fail('invalid_position', `Position ${line}:${character} is outside ${file}`); }
}
function positionResults(ts, fn, input, root) {
  const file = targetFile(input, root);
  const offset = offsetFor(ts, service, file, input.line, input.character);
  const values = fn(file, offset) ?? [];
  return values.slice(0, MAX_RESULTS).map((value) => location(ts, root, value.fileName, value.textSpan, value.name ?? null));
}
function location(ts, root, fileName, span, name = null) {
  const file = canonical(fileName);
  assertInside(root, file, 'semantic result');
  if (ignored(file)) fail('ignored_result', `Adapter returned an ignored/generated path: ${file}`);
  const source = service.getProgram()?.getSourceFile(file);
  if (!source) fail('file_not_in_project', `Semantic result is outside selected project: ${file}`);
  const start = source.getLineAndCharacterOfPosition(span.start);
  const end = source.getLineAndCharacterOfPosition(span.start + span.length);
  return { path: normalize(path.relative(root, file)), range: { start, end }, name, source_hash: hash(fs.readFileSync(file)) };
}
function diagnostics(ts, service, input, root, files) {
  const selected = input.path ? [targetFile(input, root)] : files;
  const output = [];
  for (const file of selected) {
    const source = service.getProgram()?.getSourceFile(file);
    if (!source) continue;
    const values = [...service.getSyntacticDiagnostics(file), ...service.getSemanticDiagnostics(file), ...service.getSuggestionDiagnostics(file)];
    for (const value of values) {
      const startOffset = value.start ?? 0;
      const endOffset = startOffset + (value.length ?? 0);
      output.push({
        path: normalize(path.relative(root, file)),
        range: { start: source.getLineAndCharacterOfPosition(startOffset), end: source.getLineAndCharacterOfPosition(endOffset) },
        category: ts.DiagnosticCategory[value.category].toLowerCase(),
        code: value.code,
        message: flatten(ts, value.messageText),
        source_hash: hash(fs.readFileSync(file)),
      });
      if (output.length >= MAX_RESULTS) return output;
    }
  }
  return output;
}
function workspaceSymbols(ts, service, query, root, files) {
  const needle = String(query).toLowerCase();
  const output = [];
  for (const file of files) {
    const tree = service.getNavigationTree(file);
    if (!tree) continue;
    visit(tree, file);
    if (output.length >= MAX_RESULTS) break;
  }
  return output;
  function visit(item, file) {
    if (!needle || item.text.toLowerCase().includes(needle)) {
      for (const span of item.spans ?? []) {
        output.push({ ...location(ts, root, file, span, item.text), kind: item.kind });
        if (output.length >= MAX_RESULTS) return;
      }
    }
    for (const child of item.childItems ?? []) { visit(child, file); if (output.length >= MAX_RESULTS) return; }
  }
}
function rename(ts, service, input, root) {
  const file = targetFile(input, root);
  const offset = offsetFor(ts, service, file, input.line, input.character);
  const newName = String(input.new_name ?? '');
  if (!/^[$A-Z_a-z][$\w]*$/u.test(newName)) fail('invalid_identifier', `Invalid TypeScript/JavaScript identifier: ${newName}`);
  const info = service.getRenameInfo(file, offset, { allowRenameOfImportPath: false });
  if (!info.canRename) fail('rename_refused', info.localizedErrorMessage ?? 'TypeScript refused rename');
  const locations = service.findRenameLocations(file, offset, false, false, true) ?? [];
  if (!locations.length) fail('rename_refused', 'TypeScript returned no rename locations');
  const edits = locations.map((item) => {
    const target = canonical(item.fileName);
    assertInside(root, target, 'rename location');
    if (ignored(target)) fail('ignored_result', `Rename includes ignored/generated file: ${target}`);
    const content = fs.readFileSync(target, 'utf8');
    const expected = content.slice(item.textSpan.start, item.textSpan.start + item.textSpan.length);
    return { path: normalize(path.relative(root, target)), start_byte: Buffer.byteLength(content.slice(0, item.textSpan.start)), end_byte: Buffer.byteLength(content.slice(0, item.textSpan.start + item.textSpan.length)), expected, replacement: `${item.prefixText ?? ''}${newName}${item.suffixText ?? ''}`, source_hash: hash(content) };
  });
  edits.sort((a, b) => a.path.localeCompare(b.path) || a.start_byte - b.start_byte);
  return { display_name: info.displayName, full_display_name: info.fullDisplayName, edits };
}
function fingerprint(root, files) {
  const digest = crypto.createHash('sha256');
  for (const file of [...files].sort()) { assertInside(root, file, 'project file'); digest.update(normalize(path.relative(root, file))); digest.update('\0'); digest.update(hash(fs.readFileSync(file))); digest.update('\n'); }
  return digest.digest('hex');
}
function hash(value) { return crypto.createHash('sha256').update(value).digest('hex'); }
function flatten(ts, value) { return ts.flattenDiagnosticMessageText(value, '\n'); }
function fail(code, message) { process.stdout.write(JSON.stringify({ error: { code, message } })); process.exit(2); }
