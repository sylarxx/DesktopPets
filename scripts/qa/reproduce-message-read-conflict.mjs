// Runs the actual baseline and candidate action/service declarations against a
// controlled backend contract. No real account, message or network is touched.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';

const compile = source => ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
}).outputText;
function load(source, dependencies = {}) {
  const exports = {};
  vm.runInNewContext(compile(source), {
    exports, Date, console,
    require(name) {
      assert.ok(name in dependencies, `Unexpected dependency: ${name}`);
      return dependencies[name];
    },
  });
  return exports;
}
function declaration(source, name) {
  const script = source.split('<script setup lang="ts">')[1].split('</script>')[0];
  const ast = ts.createSourceFile('App.ts', script, ts.ScriptTarget.ES2022, true);
  const node = ast.statements.find(n => ts.isFunctionDeclaration(n) && n.name?.text === name);
  assert.ok(node, `Missing ${name}`);
  return node.getText(ast);
}

const baseline = {
  service: execFileSync('git', ['show', '107f3f4:src/services/sys-message.service.ts'], { encoding: 'utf8' }),
  app: execFileSync('git', ['show', '107f3f4:src/App.vue'], { encoding: 'utf8' }),
};
const candidate = {
  service: fs.readFileSync('src/services/sys-message.service.ts', 'utf8'),
  app: fs.readFileSync('src/App.vue', 'utf8'),
};
const expiry = load(fs.readFileSync('src/utils/sys-message-expiry.ts', 'utf8'));
const feedback = load(fs.readFileSync('src/utils/sys-message-action-error.ts', 'utf8'));

function harness(source, { readId = 101, failure } = {}) {
  const calls = [];
  const request = {
    async put(path, body) {
      calls.push({ method: 'PUT', path, body });
      if (failure) throw failure;
      return false;
    },
    async get(path, options) {
      calls.push({ method: 'GET', path, options });
      return { rows: [{ id: readId, msgStatus: 1 }], total: 1 };
    },
  };
  const { sysMessageService } = load(source.service, {
    './request': { request },
    '../utils/env': { env: { enableMock: false, sysMessageWsBaseUrl: '' } },
    '../utils/sys-message-expiry': expiry,
    './diagnostic.service': { maskDiagnosticIdentifier: () => '', recordDesktopDiagnostic() {} },
  });
  const message = {
    id: '101', rawId: 101, dedupeKey: '101', msgStatus: 0,
    msgSubject: '任务即将结束', msgContent: 'controlled fixture', msgType: 1,
  };
  const current = { value: message }, error = { value: '' }, pending = { value: '' };
  const context = {
    sysMessageService, currentSysMessage: current, sysMessageActionError: error,
    sysMessageReadPendingKey: pending, isSysMessagePreview: false,
    isCurrentSysMessageReadPending: { get value() { return current.value?.dedupeKey === pending.value; } },
    sysMessageEnrichmentGeneration: 1,
    hideCurrentSysMessage(selected) { if (current.value?.id === selected.id) current.value = null; },
    ...feedback,
    console: { warn() {} },
  };
  const handle = new Function(...Object.keys(context), compile(declaration(source.app, 'handleSysMessageRead'))
    + '\nreturn handleSysMessageRead;')(...Object.values(context));
  return { message, current, error, pending, calls, run: () => handle(message) };
}

const cases = [];
const old = harness(baseline);
await old.run();
assert.equal(old.current.value, old.message);
assert.equal(old.message.msgStatus, 0);
assert.match(old.error.value, /请检查网络后重试/);
assert.equal(old.calls.length, 1);
cases.push({ name: 'v1.0.54-already-read-retry', cardRemains: true, misleadingNetworkError: true });

const repaired = harness(candidate);
await repaired.run();
assert.equal(repaired.current.value, null);
assert.equal(repaired.message.msgStatus, 1);
assert.equal(repaired.error.value, '');
assert.equal(repaired.calls[1].options.params.msgStatus, 1);
cases.push({ name: 'candidate-already-read-retry', cardRemovedAfterExplicitReadback: true });

const missing = harness(candidate, { readId: 202 });
await missing.run();
assert.equal(missing.current.value, missing.message);
assert.equal(missing.message.msgStatus, 0);
assert.match(missing.error.value, /后台尚未确认/);
cases.push({ name: 'wrong-or-missing-id', neverInfersReadFromEmptyUpdate: true });

const unauthorized = harness(candidate, { failure: Object.assign(new Error('session rejected'), { status: 401 }) });
await unauthorized.run();
assert.equal(unauthorized.current.value, unauthorized.message);
assert.equal(unauthorized.calls.length, 1);
assert.match(unauthorized.error.value, /登录状态已失效/);
cases.push({ name: 'expired-token', loginErrorDistinctFromNetwork: true });

const timeout = harness(candidate, { failure: new Error('连接后台服务超时，操作结果尚未确认') });
await timeout.run();
assert.equal(timeout.message.msgStatus, 0);
assert.match(timeout.error.value, /已读结果尚未确认/);
assert.equal(timeout.pending.value, '');
cases.push({ name: 'timed-out-write', doesNotClaimFailedOrRead: true, controlsReleased: true });

// Startup must honor the close record before any notification-ready callback.
const restoreContext = {
  userStore: { userInfo: { userId: 'same-user' } },
  isSysMessageDismissed: (_owner, message) => message.id === 'closed',
  ...expiry,
};
const restore = new Function(...Object.keys(restoreContext), compile(declaration(candidate.app, 'canRestoreSysMessage'))
  + '\nreturn canRestoreSysMessage;')(...Object.values(restoreContext));
assert.equal(restore({ id: 'closed', dedupeKey: 'closed', expiresAt: Date.now() + 1000 }), false);
assert.equal(restore({ id: 'new', dedupeKey: 'new', expiresAt: Date.now() + 1000 }), true);
assert.equal(restore({ dedupeKey: 'corrupt', expiresAt: Date.now() + 1000 }), false);
cases.push({ name: 'restart-with-old-snapshot', dismissedHeadFilteredBeforeInitialShow: true, corruptSnapshotRejected: true });

const report = {
  passed: true, baselineCommit: '107f3f4', source: 'actual App.vue and sys-message.service.ts',
  caseCount: cases.length, cases,
  limits: ['Controlled backend fixture; no live API response.', 'No Windows renderer or compositor is reproduced.'],
};
fs.mkdirSync('artifacts/message-read-conflict-qa', { recursive: true });
fs.writeFileSync('artifacts/message-read-conflict-qa/report.json', JSON.stringify(report, null, 2));
console.log(JSON.stringify(report, null, 2));
