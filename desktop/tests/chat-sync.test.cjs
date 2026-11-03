const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { resolve } = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const source = readFileSync(resolve(__dirname, '../app/ui/chat.js'), 'utf8');
const merge = source.slice(source.indexOf('  function applyRunSync('), source.indexOf('  listen("wiqo",', source.indexOf('  function applyRunSync(')));
const apply = source.slice(source.indexOf('  function applyChatEvent('), source.indexOf('  // ----- composer -----'));
const dispatch = source.slice(source.indexOf('    const payload = event.payload;', source.indexOf('  listen("wiqo",')), source.indexOf('    if (payload.type === "thread"'));
function harness() {
  const calls = [];
  const context = vm.createContext({
    messages: [], conversations: [], conversation: 'conversation-1', turn: null, task: null,
    latestSync: null, syncAccount: 'owner@test.invalid', syncHistoryKey: '', remoteTask: null, remoteAgent: null,
    render() {}, renderList() {}, note() {}, finishTrace() {}, loadConversations() {}, askFor() {},
    newTrace() { return { steps: [] }; },
    open(...args) { calls.push(['history', ...args]); },
    invoke(...args) { calls.push(['invoke', ...args]); return Promise.resolve(); },
    locate(...args) { calls.push(['locate', ...args]); },
  });
  vm.runInContext(`${merge}\n${apply}\nfunction dispatch(event) { ${dispatch} }`, context);
  return { context, calls };
}
function snapshot(overrides = {}) {
  return { type: 'snapshot', conversations: [], requests: [{ requestId: 'request-1', taskId: 'task-1', conversationId: 'conversation-1',
    revision: 1, status: 'running', goal: 'Görev', response: { reply: 'İlerliyor', widgets: [], backendConfirmations: [] }, ...overrides }] };
}

test('an observer never reads or posts its own location; the originating client keeps its existing flow', () => {
  const { context, calls } = harness();
  const event = { type: 'location_required', requestId: 'weather-1', prompt: 'Hangi şehir?' };
  const remote = { trace: {}, text: '' };
  context.applyChatEvent(remote, event, false);
  assert.equal(calls.length, 0);
  assert.equal(remote.location.state, 'ask');
  context.applyChatEvent({ trace: {}, text: '' }, event, true);
  assert.equal(calls[0][0], 'locate');
});

test('the same remote task merges in place and a late snapshot cannot reopen a cancellation', () => {
  const { context } = harness();
  context.applyRunSync(snapshot(), false);
  context.applyRunSync(snapshot({ revision: 2 }), false);
  assert.equal(context.messages.length, 2);
  context.applyRunSync(snapshot({ revision: 3, cancelled: true }), false);
  context.applyRunSync(snapshot({ revision: 2, cancelled: false }), false);
  assert.equal(context.messages[1].cancelled, true);
  assert.equal(context.remoteTask, null);
  const before = context.messages[1].text;
  context.dispatch({ payload: { type: 'runSync', account: 'owner@test.invalid', event: { type: 'copilot', conversationId: 'conversation-1', requestId: 'request-1', event: { type: 'delta', text: 'late' } } } });
  assert.equal(context.messages[1].text, before);
});

test('an event queued for a signed-out account cannot reach the new account', () => {
  const { context } = harness();
  context.dispatch({ payload: { type: 'runSync', account: 'old@test.invalid', event: snapshot() } });
  assert.equal(context.messages.length, 0);
});

test('a task finished while offline refreshes existing history without navigating the user', () => {
  const { context, calls } = harness();
  const event = snapshot({ status: 'done' });
  event.conversations = [{ id: 'conversation-1', updatedAt: '2026-11-03', turnCount: 2 }];
  context.applyRunSync(event);
  assert.deepEqual(calls[0], ['history', 'conversation-1', true]);
});

test('a bounded sync preview cannot replace a complete local reply', () => {
  const { context } = harness();
  context.applyRunSync(snapshot(), false);
  context.messages[1].text = 'complete reply';
  context.applyRunSync(snapshot({ revision: 2, response: { reply: 'short', truncated: true } }), false);
  assert.equal(context.messages[1].text, 'complete reply');
});
