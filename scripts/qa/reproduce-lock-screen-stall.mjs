// Investigation probe for v1.0.53, not a specification of desired behavior.
// Runs actual source declarations with simulated IPC and a deterministic clock.
// Does not claim to reproduce Windows locking, WebView2 or native pixels.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const baseline = '65ed423b5a92642cc0242dadcdb335002284dcfd';
const read = (file) => execFileSync('git', ['show', `${baseline}:${file}`], { cwd: root, encoding: 'utf8' });
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
  const visible = { value: null };
  const calls = { published: [], shows: 0, hides: 0, panelOpens: 0, fallback: [] };
  let nativeVisible = false;
  let delivery;
  const state = {
    ...time.globals,
    createNotificationDelivery, canOpenMascotTodoPanel, isSysMessageExpired,
    currentSysMessage: current, sysMessageQueue: queue, visibleSystemNotification: visible,
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
      'showIncomingSysMessage', 'showNextSysMessage', 'expireStaleSysMessages']
      .map((name) => declaration('src/App.vue', name)),
    declaration('src/views/MascotWindow.vue', 'togglePanel'),
  ].join('\n');
  const context = vm.createContext(state);
  const api = vm.runInContext(compile(`${declarations}\nglobalThis.probe = {
    notificationDelivery, syncSystemNotificationWindow, togglePanel,
    showIncomingSysMessage, expireStaleSysMessages
  };`), context) || context.probe;
  delivery = context.probe.notificationDelivery;
  return {
    ...api, time, current, queue, visible, calls,
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
  assert.equal(h.calls.published.length, 3);
  assert.equal(h.calls.fallback.length, 1);
  assert.equal(h.nativeVisible, false);
  assert.equal(h.visible.value, null);
  assert.equal(h.current.value.id, 'unread-a');
  h.togglePanel(); assert.equal(h.calls.panelOpens, 0);
  // A delayed ACK and the same sync call used by the ready listener cannot revive it.
  h.setAcknowledgement(true);
  h.notificationDelivery.acknowledge(h.calls.published.at(-1).generation);
  await h.syncSystemNotificationWindow(); await h.time.tick(0);
  // A fresh reminder queues behind the retained invisible head, without a new attempt.
  h.showIncomingSysMessage({
    id: 'unread-b', dedupeKey: 'unread-b', displayContent: 'New reminder',
    expiresAt: h.time.now() + 30 * 60_000,
  });
  await h.syncSystemNotificationWindow(); await h.time.tick(1000);
  assert.equal(h.queue.value.length, 1);
  assert.equal(h.calls.shows, 0);
  assert.equal(h.calls.published.length, 3);
  h.togglePanel(); assert.equal(h.calls.panelOpens, 0);
  // It is not forever when the mascot JS is healthy: real expiry code clears the head.
  h.time.jumpWithoutCallbacks(31 * 60_000);
  h.expireStaleSysMessages();
  await h.syncSystemNotificationWindow(); await h.time.tick(0);
  h.togglePanel(); assert.equal(h.calls.panelOpens, 1);
  return {
    attempts: 3, lateAckAndReadySyncRecovered: false,
    invisibleHeadBlockedClickAndNextMessage: true,
    expiryClearsBlockIfMascotJsExecutes: true,
  };
});

await observe('scheduler-gap-during-delivery', async (h) => {
  await h.syncSystemNotificationWindow(); await h.time.tick(0);
  // Model only a scheduling gap: clock advances while callbacks cannot run.
  h.time.jumpWithoutCallbacks(60_000);
  await h.time.tick(0);
  assert.equal(h.calls.fallback.length, 1);
  h.setAcknowledgement(true);
  h.notificationDelivery.acknowledge(h.calls.published[0].generation);
  await h.syncSystemNotificationWindow(); await h.time.tick(0);
  h.togglePanel();
  assert.equal(h.calls.shows, 0);
  assert.equal(h.calls.panelOpens, 0);
  return { simulatedGapMs: 60_000, attemptsBeforeStop: h.calls.published.length, recovered: false };
});

await observe('lost-surface-after-success', async (h) => {
  await h.syncSystemNotificationWindow(); await h.time.tick(0);
  assert.equal(h.calls.shows, 1);
  h.loseNativeSurface();
  await h.syncSystemNotificationWindow(); await h.time.tick(60_000);
  h.current.value = { ...h.current.value, displayContent: 'Updated content' };
  await h.syncSystemNotificationWindow(); await h.time.tick(0);
  assert.equal(h.nativeVisible, false);
  assert.equal(h.visible.value.kind, 'message');
  assert.equal(h.calls.shows, 1);
  h.togglePanel(); assert.equal(h.calls.panelOpens, 0);
  return { nativeSurfaceLossInjected: true, logicalVisibleRemainsTrue: true, nativeReShowCalls: 0 };
});

// A new coordinator, as created by app restart, has a fresh budget for the same unread id.
await observe('healthy-new-coordinator-after-restart', async (h) => {
  await h.syncSystemNotificationWindow(); await h.time.tick(0);
  assert.equal(h.nativeVisible, true);
  return { sameUnreadMessageShownWithNewCoordinator: true };
});

const report = {
  kind: 'source-level-fault-injection',
  packageVersion: JSON.parse(read('package.json')).version,
  sourceSha256: Object.fromEntries([
    'src/App.vue', 'src/views/MascotWindow.vue', 'src/utils/notification-delivery.ts',
    'src/utils/mascot-panel-access.ts', 'src/utils/sys-message-expiry.ts',
  ].map((file) => [file, createHash('sha256').update(read(file)).digest('hex')])),
  baselineCommit: baseline,
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
const output = path.join(root, 'artifacts/lock-screen-investigation-20260922/source-probe.json');
fs.mkdirSync(path.dirname(output), { recursive: true });
fs.writeFileSync(output, `${JSON.stringify(report, null, 2)}\n`);
console.log(JSON.stringify(report, null, 2));
