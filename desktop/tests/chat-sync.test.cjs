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
    render() {}, renderList() {}, queueRender() {}, note() {}, finishTrace() {}, loadConversations() {}, askFor() {},
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

function runEvent(context, seq, event) {
  context.dispatch({ payload: { type: 'runSync', account: 'owner@test.invalid',
    event: { type: 'copilot', conversationId: 'conversation-1', requestId: 'request-1', seq, event },
  } });
}

test('run sync rejects duplicate deltas and waits for a snapshot before applying text after a gap', () => {
  const { context } = harness();
  context.applyRunSync(snapshot({ response: { reply: 'a', seq: 1, steps: [{ id: 'stale-step' }] } }), false);
  const reply = context.messages[1];
  runEvent(context, 2, { type: 'delta', text: 'b' });
  runEvent(context, 2, { type: 'delta', text: 'b' });
  runEvent(context, 4, { type: 'reset' });
  runEvent(context, 5, { type: 'delta', text: 'unsafe' });
  assert.equal(reply.text, 'ab');
  context.applyRunSync(snapshot({ response: { reply: 'a', seq: 1 } }), false);
  assert.equal(reply.text, 'ab');
  assert.equal(context.remoteTask, 'task-1');
  context.applyRunSync(snapshot({ response: { reply: 'recovered', seq: 5, steps: [] } }), false);
  runEvent(context, 6, { type: 'delta', text: '!' });
  assert.equal(reply.text, 'recovered!');
  assert.equal(reply.steps.length, 0);
});

test('an initial gap and legacy deltas cannot append to a numbered response', () => {
  const { context } = harness();
  context.applyRunSync(snapshot(), false);
  runEvent(context, 3, { type: 'delta', text: 'missed 1 and 2' });
  assert.equal(context.messages[1].text, 'İlerliyor');
  context.applyRunSync(snapshot({ response: { reply: 'recovered', seq: 3 } }), false);
  runEvent(context, undefined, { type: 'delta', text: 'unnumbered' });
  assert.equal(context.messages[1].text, 'recovered');
});

test('a canonical cancellation survives a late success and ignores later stream text', () => {
  const { context } = harness();
  context.applyRunSync(snapshot({ response: { reply: 'running', seq: 1 } }), false);
  context.applyRunSync(snapshot({ revision: 2, status: 'done', response: { reply: 'partial', seq: 2, outcome: { state: 'cancelled', verified: false } } }), false);
  context.applyRunSync(snapshot({ revision: 2, status: 'done', response: { reply: 'late', seq: 3, outcome: { state: 'done', verified: true } } }), false);
  runEvent(context, 3, { type: 'delta', text: 'late event' });
  assert.equal(context.messages[1].text, 'partial');
  assert.equal(context.messages[1].cancelled, true);
});

test('a resumed task keeps its existing bubbles and starts sequencing its new request', () => {
  const { context } = harness();
  context.applyRunSync(snapshot({ response: { reply: 'first run', seq: 5 } }), false);
  context.applyRunSync(snapshot({ status: 'running', response: { reply: 'paused', seq: 7, outcome: { state: 'paused' } } }), false);
  context.applyRunSync(snapshot({ response: { reply: 'first run', seq: 5 } }), false);
  assert.equal(context.messages[1].text, 'paused');
  const resume = snapshot({ requestId: 'request-2', response: { reply: 'resumed', seq: 1 } });
  resume.requests.push(snapshot({ status: 'done' }).requests[0]);
  context.applyRunSync(resume, false);
  assert.equal(context.messages.length, 2);
  assert.equal(context.messages[1].requestId, 'request-2');
  assert.equal(context.messages[1].text, 'resumed');
  assert.equal(context.messages[1].syncSeq, 1);
});
