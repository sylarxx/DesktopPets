// Executes actual service and App declarations against a simulated backend.
// It verifies the affected state transitions, not a live server response or
// Windows/WebView2 behavior.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const baseline = execFileSync('git', ['rev-parse', '107f3f4'], { cwd: root, encoding: 'utf8' }).trim();
const read = file => fs.readFileSync(path.join(root, file), 'utf8');
const old = file => execFileSync('git', ['show', `${baseline}:${file}`], { cwd: root, encoding: 'utf8' });
const compile = source => ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
}).outputText;
const script = source => source.includes('<script setup lang="ts">')
  ? source.split('<script setup lang="ts">')[1].split('</script>')[0] : source;
const astFor = source => ts.createSourceFile('probe.ts', script(source), ts.ScriptTarget.ES2022, true);
function selected(source, names, variables = []) {
  const ast = astFor(source);
  const result = [];
  for (const node of ast.statements) {
    if (ts.isFunctionDeclaration(node) && names.includes(node.name?.text)) result.push(node.getText(ast));
    if (ts.isVariableStatement(node)) for (const declaration of node.declarationList.declarations) {
      if (variables.includes(declaration.name.getText(ast))) result.push(`const ${declaration.getText(ast)};`);
    }
  }
  assert.equal(result.length, names.length + variables.length, 'all requested declarations must exist');
  return result.join('\n');
}
function execute(source, context, result) {
  return new Function(...Object.keys(context), compile(source) + '\n' + result)(...Object.values(context));
}
function moduleFrom(source, context = {}) {
  const ast = astFor(source);
  const body = ast.statements.filter(node => !ts.isImportDeclaration(node)).map(node => node.getText(ast)).join('\n');
  const exports = {};
  execute(body, { ...context, exports }, '');
  return exports;
}
const expiry = moduleFrom(read('src/utils/sys-message-expiry.ts'));
const values = new Map();
const localStorage = {
  getItem: key => values.get(key) ?? null,
  setItem: (key, value) => values.set(key, value),
  removeItem: key => values.delete(key),
};
const dismissal = moduleFrom(read('src/utils/sys-message-dismissal.ts'), { ...expiry, localStorage });
const errors = moduleFrom(read('src/utils/sys-message-action-error.ts'));
const message = id => ({
  id: String(id), rawId: id, dedupeKey: String(id), msgSubject: '模拟提醒',
  msgContent: '', msgStatus: 0, msgType: 1, expiresAt: Date.now() + 60_000,
});
function serviceFrom(source, request) {
  return moduleFrom(source, {
    request, ...expiry, env: { enableMock: false, sysMessageWsBaseUrl: '' },
    recordDesktopDiagnostic() {}, maskDiagnosticIdentifier: value => value,
  }).sysMessageService;
}
function actionsFrom(source, service) {
  const current = { value: message(101) }, queue = { value: [] }, deferred = { value: [] };
  const pending = { value: '' }, allPending = { value: false }, error = { value: '' }, owner = { value: 'account-one' };
  const names = ['handleSysMessageRead', 'hideCurrentSysMessage', 'showNextSysMessage'];
  if (source.includes('function handleSysMessageDismiss(')) names.push('handleSysMessageDismiss', 'isLocallyDismissed');
  const context = {
    currentSysMessage: current, sysMessageQueue: queue, deferredSysMessages: deferred,
    sysMessageReadPendingKey: pending, sysMessageReadAllPending: allPending, sysMessageActionError: error,
    sysMessageUserId: owner, sysMessageService: service, ...expiry, ...dismissal, ...errors,
    sysMessageEnrichmentGeneration: 1, isSysMessagePreview: false,
    isCurrentSysMessageReadPending: { get value() { return allPending.value || current.value?.dedupeKey === pending.value; } },
    emitTo() {}, hidePanelWindow() {}, deliverTasksWhenSystemMessagesFinish() {}, console: { warn() {} },
  };
  const actions = execute(selected(source, names), context,
    `return {${names.join(',')}, changeSession() { sysMessageEnrichmentGeneration += 1; }};`);
  return { ...actions, current, queue, deferred, pending, error, owner };
}
function alreadyReadBackend() {
  const calls = [];
  return {
    calls,
    async put(route, body) {
      calls.push({ method: 'PUT', route, ids: body.ids });
      // SysMessageMapper only updates msg_status=0, so an already-read row
      // produces zero affected rows and the service returns false.
      return false;
    },
    async get(route, options) {
      calls.push({ method: 'GET', route, params: options.params });
      return { rows: [{ id: 101, msgStatus: 1 }], total: 1 };
    },
  };
}
const beforeBackend = alreadyReadBackend();
const before = actionsFrom(old('src/App.vue'), serviceFrom(old('src/services/sys-message.service.ts'), beforeBackend));
await before.handleSysMessageRead(before.current.value);
assert.equal(before.current.value.id, '101');
assert.match(before.error.value, /未能标记已读.*网络/);
assert.equal(beforeBackend.calls.length, 1);
const afterBackend = alreadyReadBackend();
const after = actionsFrom(read('src/App.vue'), serviceFrom(read('src/services/sys-message.service.ts'), afterBackend));
await after.handleSysMessageRead(after.current.value);
assert.equal(after.current.value, null);
assert.equal(after.error.value, '');
assert.equal(afterBackend.calls.length, 2);

let finishOld, finishNext;
const service = { markRead(item) { return new Promise((resolve, reject) => {
  if (item.id === '101') finishOld = { resolve, reject };
  else finishNext = { resolve, reject };
}); } };
for (const succeeds of [false, true]) {
  const actions = actionsFrom(read('src/App.vue'), service);
  const first = actions.current.value;
  actions.queue.value = [message(102), { ...first, dedupeKey: 'another-transport' }];
  actions.deferred.value = [first];
  const oldRead = actions.handleSysMessageRead(first);
  actions.handleSysMessageDismiss(first);
  assert.equal(first.msgStatus, 0, 'close must not manufacture a backend read');
  assert.equal(actions.current.value.id, '102');
  assert.equal(actions.queue.value.length, 0);
  assert.equal(actions.deferred.value.length, 0);
  assert.equal(dismissal.isSysMessageDismissed('account-one', first), true);
  assert.equal(dismissal.isSysMessageDismissed('account-two', first), false);
  const nextRead = actions.handleSysMessageRead(actions.current.value);
  if (succeeds) finishOld.resolve(true); else finishOld.reject(new Error('late write failure'));
  await oldRead;
  assert.equal(actions.current.value.id, '102');
  assert.equal(actions.pending.value, '102');
  assert.equal(actions.error.value, '');
  finishNext.resolve(true);
  await nextRead;
  assert.equal(actions.current.value, null);
}
const restoreNames = ['currentSysMessage', 'sysMessageQueue', 'deferredSysMessages'];
const saved = { current: message(101), queue: [message(102), message(101)], deferred: [message(101)] };
const restoreCode = selected(read('src/App.vue'), ['canRestoreSysMessage'], restoreNames);
const restored = execute(restoreCode, {
  savedMascot: saved, ref: value => ({ value }), userStore: { userInfo: { userId: 'account-one' } },
  ...expiry, ...dismissal,
}, 'return { currentSysMessage, sysMessageQueue, deferredSysMessages };');
assert.equal(restored.currentSysMessage.value, null);
assert.deepEqual(restored.sysMessageQueue.value.map(item => item.id), ['102']);
assert.equal(restored.deferredSysMessages.value.length, 0);
const malformed = execute(restoreCode, {
  savedMascot: { current: { expiresAt: Date.now() + 60_000 }, queue: [null], deferred: [{}] },
  ref: value => ({ value }), userStore: { userInfo: { userId: 'account-one' } }, ...expiry, ...dismissal,
}, 'return { currentSysMessage, sysMessageQueue, deferredSysMessages };');
assert.equal(malformed.currentSysMessage.value, null);
assert.deepEqual(malformed.sysMessageQueue.value, []);
assert.deepEqual(malformed.deferredSysMessages.value, []);

const precisionCalls = [];
const precisionService = serviceFrom(read('src/services/sys-message.service.ts'), {
  async put(_route, body) { precisionCalls.push(body.ids); return true; },
});
await precisionService.markRead(message('9007199254740993'));
assert.deepEqual(precisionCalls[0], ['9007199254740993']);
const numericWire = JSON.parse('{"id":9007199254740993}').id;
assert.equal(String(numericWire), '9007199254740992');

const sourceFiles = ['src/App.vue', 'src/services/sys-message.service.ts', 'src/utils/sys-message-dismissal.ts'];
const report = {
  passed: true, baseline, scope: 'actual source declarations with simulated HTTP/backend state; no live response or Windows claim',
  before: { cardRetained: true, error: before.error.value, requests: beforeBackend.calls },
  after: { cardRemoved: true, error: after.error.value, requests: afterBackend.calls },
  cases: ['already-read false write reconciles by authenticated exact ID', 'close preserves unread and survives restart',
    'close removes copies in queued/deferred transports', 'old success and failure cannot erase the next card or pending state',
    'dismissals are per account', 'restored snapshots filter dismissed and malformed identities before presentation',
    'large IDs received as strings preserve precision'],
  remainingRisk: 'JSON numeric IDs above Number.MAX_SAFE_INTEGER lose precision before normalization; current backend uses generated IDs, live magnitude unverified',
  sourceSha256: Object.fromEntries(sourceFiles.map(file => [file, createHash('sha256').update(read(file)).digest('hex')])),
};
const out = path.join(root, 'artifacts/message-read-investigation-20261009');
fs.mkdirSync(out, { recursive: true });
fs.writeFileSync(path.join(out, 'source-probe.json'), JSON.stringify(report, null, 2) + '\n');
console.log(JSON.stringify(report, null, 2));
