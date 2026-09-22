// Regression checks for the actual notification, queue and click recovery path.
// Runs actual source declarations with simulated IPC and a deterministic clock.
// Does not claim to reproduce Windows locking, WebView2 or native pixels.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const read = (file) => fs.readFileSync(path.join(root, file), 'utf8');
const compile = (source) => ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
}).outputText;
const flush = async () => { for (let i = 0; i < 20; i += 1) await Promise.resolve(); };

function declaration(file, name, variable = false) {
  const source = read(file).replace(/^[\s\S]*?<script setup lang="ts">/, '').split('</script>')[0];
  const ast = ts.createSourceFile(file, source, ts.ScriptTarget.ES2022, true);
  for (const node of ast.statements) {
    if (!variable && ts.isFunctionDeclaration(node) && node.name?.text === name) return node.getText(ast);
    if (variable && ts.isVariableStatement(node)) {
      const match = node.declarationList.declarations.find((item) => item.name.getText(ast) === name);
      if (match) return `const ${match.getText(ast)};`;
    }
  }
  throw new Error(`Missing source declaration ${name} in ${file}`);
}

function clock() {
  let now = Date.UTC(2026, 8, 22, 1);
  let sequence = 0;
  const timers = new Map();
  class ClockDate extends Date {
    constructor(...args) { if (args.length) super(...args); else super(now); }
    static now() { return now; }
  }
  const timerApi = {
    setTimeout(callback, delay = 0) {
      const id = ++sequence;
      timers.set(id, { at: now + Math.max(0, delay), callback });
      return id;
    },
    clearTimeout(id) { timers.delete(id); },
  };
  return {
    globals: { Date: ClockDate, ...timerApi, window: timerApi },
    now: () => now,
    jumpWithoutCallbacks(ms) { now += ms; },
    async tick(ms) {
      const until = now + ms;
      await flush();
      for (let steps = 0; steps < 1000; steps += 1) {
        const next = [...timers].sort((a, b) => a[1].at - b[1].at || a[0] - b[0])[0];
        if (!next || next[1].at > until) { now = until; await flush(); return; }
        timers.delete(next[0]);
        now = Math.max(now, next[1].at);
        next[1].callback();
        await flush();
      }
      throw new Error('Unbounded timer loop in probe');
    },
  };
}

function loadUtility(file, globals) {
  const exports = {};
  vm.runInNewContext(compile(read(file)), { ...globals, exports }, { filename: file });
  return exports;
}

function harness({ acknowledge = true } = {}) {
  const time = clock();
  const { createNotificationDelivery } = loadUtility('src/utils/notification-delivery.ts', time.globals);
  const { canOpenMascotTodoPanel } = loadUtility('src/utils/mascot-panel-access.ts', time.globals);
  const { isSysMessageExpired } = loadUtility('src/utils/sys-message-expiry.ts', time.globals);
  const current = { value: {
    id: 'unread-a', dedupeKey: 'unread-a', displayContent: 'Unprocessed reminder',
    expiresAt: time.now() + 30 * 60_000,
  } };
  const queue = { value: [] };
  const deferred = { value: [] };
  const visible = { value: null };
  const calls = { published: [], shows: 0, hides: 0, panelOpens: 0, fallback: [], repairs: 0, reconnects: 0 };
  let nativeVisible = false;
  let delivery;
  const state = {
    ...time.globals,
    createNotificationDelivery, canOpenMascotTodoPanel, isSysMessageExpired,
    currentSysMessage: current, sysMessageQueue: queue, visibleSystemNotification: visible,
    deferredSysMessages: deferred, runtimeInteractive: { value: true }, runtimeRecovered: false,
    sysMessageExpiryTimer: undefined,
    requestNotificationRecovery() { calls.repairs += 1; }, persistRecoveryState() {},
    connectDesktopSockets() { calls.reconnects += 1; }, requestPanelTaskState() {},
    needsAuth: { value: false }, authPending: { value: false }, authErrorMessage: { value: '' },
    contextMenuWindowVisible: { value: false }, windowMode: 'mascot',
    systemNotificationSyncGeneration: 0, systemNotificationMessageKey: '',
    systemNotificationPresentationGeneration: 0, userHiddenSystemNotificationKey: '',
    currentSysMessageContent: { get value() { return current.value?.displayContent ?? ''; } },
    pendingSysMessageCount: { get value() { return queue.value.length; } },
    isCurrentSysMessageReadPending: { value: false },
    sysMessageReadAllPending: { value: false }, sysMessageActionError: { value: '' },
    MASCOT_SYSTEM_NOTIFICATION_PRESENT_EVENT: 'present',
    recordDesktopDiagnostic() {},
    async emitTo(_target, event, payload) {
      if (event !== 'present') return;
      calls.published.push(payload);
      if (acknowledge && payload.presentation) delivery.acknowledge(payload.generation);
    },
    async showNotificationWindow() { return true; },
    async showMascotSystemNotificationWindow() { calls.shows += 1; nativeVisible = true; return true; },
    async hideMascotSystemNotificationWindow() { calls.hides += 1; nativeVisible = false; return true; },
    mascotStore: { showMessage(...args) { calls.fallback.push(args); } },
    props: {
      needsAuth: false,
      get sysMessage() { return current.value; },
      get systemMessageVisible() { return visible.value?.kind === 'message'; },
    },
    panelVisible: { value: false }, panelHasText: { value: false },
    emit() {}, dismissTransientOverlays() {}, playTransientAnimation() {},
    mascotWaitingInteractionMs: 0,
    togglePanelWindow() { calls.panelOpens += 1; },
    hidePanelWindow() {}, deliverTasksWhenSystemMessagesFinish() {},
  };
  const declarations = [
    declaration('src/App.vue', 'notificationDelivery', true),
    ...['buildSystemNotificationPresentation', 'syncSystemNotificationWindow',
      'showIncomingSysMessage', 'showNextSysMessage', 'expireStaleSysMessages',
      'handleNotificationStopped', 'recoverDesktopRuntime', 'scheduleSysMessageExpiry']
      .map((name) => declaration('src/App.vue', name)),
    declaration('src/views/MascotWindow.vue', 'togglePanel'),
  ].join('\n');
  const context = vm.createContext(state);
  const api = vm.runInContext(compile(`${declarations}\nglobalThis.probe = {
    notificationDelivery, syncSystemNotificationWindow, togglePanel,
    showIncomingSysMessage, expireStaleSysMessages, recoverDesktopRuntime
  };`), context) || context.probe;
  delivery = context.probe.notificationDelivery;
  return {
    ...api, time, current, queue, deferred, visible, calls,
    setAcknowledgement(value) { acknowledge = value; },
    get nativeVisible() { return nativeVisible; },
    loseNativeSurface() { nativeVisible = false; },
    dispose() { delivery.dispose(); },
  };
}

const cases = [];
async function observe(name, run) {
  const h = harness({ acknowledge: name.startsWith('healthy') || name.startsWith('lost-surface') });
  try { cases.push({ name, ...(await run(h)) }); } finally { h.dispose(); }
}

await observe('healthy-control', async (h) => {
  await h.syncSystemNotificationWindow(); await h.time.tick(0);
  assert.equal(h.nativeVisible, true);
  h.togglePanel(); assert.equal(h.calls.panelOpens, 0);
  h.current.value = null;
  await h.syncSystemNotificationWindow(); await h.time.tick(0);
  h.togglePanel(); assert.equal(h.calls.panelOpens, 1);
  return { cardShown: true, clickWorksOnceMessageCleared: true };
});

await observe('missing-layout-ack', async (h) => {
  await h.syncSystemNotificationWindow(); await h.time.tick(10_000);
  assert.equal(h.calls.fallback.length, 1);
  assert.equal(h.nativeVisible, false);
  assert.equal(h.current.value, null);
  assert.equal(h.deferred.value.length, 1);
  h.togglePanel(); assert.equal(h.calls.panelOpens, 1);
  h.setAcknowledgement(true);
  h.showIncomingSysMessage({ id: 'unread-b', dedupeKey: 'unread-b', displayContent: 'New reminder', expiresAt: h.time.now() + 30 * 60_000 });
  await h.syncSystemNotificationWindow(); await h.time.tick(0);
  assert.equal(h.nativeVisible, true);
  assert.equal(h.current.value.id, 'unread-b');
  assert.equal(h.calls.repairs, 1);
  return { clickRestored: true, nextMessageShown: true, failedMessagePreserved: true };
});

await observe('scheduler-gap-during-delivery', async (h) => {
  await h.syncSystemNotificationWindow(); await h.time.tick(0);
  h.time.jumpWithoutCallbacks(60_000);
  await h.time.tick(0);
  assert.equal(h.calls.fallback.length, 1);
  h.setAcknowledgement(true);
  h.recoverDesktopRuntime({ epoch: 2, interactive: true, recovered: false, visible: true });
  await h.time.tick(0);
  assert.equal(h.nativeVisible, true);
  assert.equal(h.current.value.id, 'unread-a');
  assert.equal(h.calls.reconnects, 1);
  return { sameMessageRecoveredAfterSchedulingGap: true, connectionsRestored: true };
});

await observe('lost-surface-after-success', async (h) => {
  await h.syncSystemNotificationWindow(); await h.time.tick(0);
  h.loseNativeSurface();
  h.recoverDesktopRuntime({ epoch: 2, interactive: true, recovered: true, visible: true });
  await h.time.tick(0);
  assert.equal(h.nativeVisible, true);
  assert.equal(h.calls.shows, 2);
  return { sameContentRedisplayedAfterNativeRecoveryEvent: true };
});

await observe('locked-message-expiry', async (h) => {
  h.recoverDesktopRuntime({ epoch: 2, interactive: false, recovered: false, visible: true });
  await h.syncSystemNotificationWindow();
  await h.time.tick(8 * 60 * 60_000);
  assert.equal(h.calls.fallback.length, 0);
  h.setAcknowledgement(true);
  h.recoverDesktopRuntime({ epoch: 3, interactive: true, recovered: false, visible: true });
  await h.time.tick(0);
  assert.equal(h.current.value, null);
  assert.equal(h.calls.shows, 0);
  h.togglePanel(); assert.equal(h.calls.panelOpens, 1);
  return { expiredMessagesNotRedisplayed: true, lockDoesNotConsumeRetries: true };
});

// A new coordinator, as created by app restart, has a fresh budget for the same unread id.
await observe('healthy-new-coordinator-after-restart', async (h) => {
  await h.syncSystemNotificationWindow(); await h.time.tick(0);
  assert.equal(h.nativeVisible, true);
  return { sameUnreadMessageShownWithNewCoordinator: true };
});

const report = {
  kind: 'source-level-recovery-regression',
  packageVersion: JSON.parse(read('package.json')).version,
  sourceSha256: Object.fromEntries([
    'src/App.vue', 'src/views/MascotWindow.vue', 'src/utils/notification-delivery.ts',
    'src/utils/mascot-panel-access.ts', 'src/utils/sys-message-expiry.ts',
  ].map((file) => [file, createHash('sha256').update(read(file)).digest('hex')])),
  productionSourceChanged: true,
  nativeWindowsLockReproduced: false,
  caseCount: cases.length,
  cases,
  limits: [
    'IPC, native surfaces, Vue reactive refs and clock are simulated.',
    'Actual notification coordinator and extracted App.vue/MascotWindow.vue functions execute.',
    'A scheduling gap does not prove the reported machine throttled or suspended WebView2.',
    'A permanently unresponsive renderer is a separate branch and is not simulated here.',
  ],
};
const output = path.join(root, 'artifacts/runtime-recovery-qa/source-regression.json');
fs.mkdirSync(path.dirname(output), { recursive: true });
fs.writeFileSync(output, `${JSON.stringify(report, null, 2)}\n`);
console.log(JSON.stringify(report, null, 2));
