const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { resolve } = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const source = readFileSync(resolve(__dirname, '../app/ui/discover.js'), 'utf8')
  .replace(/\}\)\(\);\s*$/, 'window.testDiscover = { state, load, search, openProblem, closeSide, displayImage, paintCover }; })();');

function harness({ ready, link, imageData } = {}) {
  const elements = new Map();
  const pending = new Map();
  const mediaCalls = [];
  const coverImages = [];
  const Image = class { constructor() { coverImages.push(this); } };
  const element = (textContent = '') => ({
    textContent, dataset: {}, children: [], hidden: false,
    replaceChildren(...children) { this.children = children; },
    append(...children) { this.children.push(...children); },
    addEventListener() {}, before() {},
  });
  const getElementById = (id) => {
    if (!elements.has(id)) elements.set(id, element());
    return elements.get(id);
  };
  const window = {
    WiqoReady: ready,
    WiqoIcons: { icon: element },
    WiqoRender: { el: (_tag, _class, text) => element(text), button: (text) => element(text) },
    __TAURI__: {
      core: { invoke: (command, args) => {
        if (command === 'take_discover_link') return Promise.resolve(link);
        if (command === 'discover_image') { mediaCalls.push(args.url); return Promise.resolve(imageData); }
        if (command !== 'missions_call') return Promise.resolve(null);
        if (args.path === 'missions/notifications') return Promise.resolve({ unread: 0 });
        return new Promise((resolve, reject) => pending.set(args.path, { resolve, reject }));
      } },
      event: { listen: async () => {} },
    },
    addEventListener() {},
    showSection() { assert.fail('Keşfet is already visible'); },
  };
  const document = { getElementById, querySelectorAll: () => [], body: { dataset: { section: 'discover' } } };
  vm.runInNewContext(source, { window, document, clearTimeout, URLSearchParams, Image });
  return { ...window.testDiscover, getElementById, pending, mediaCalls, coverImages };
}

test('late feed results cannot overwrite the selected feed or its next cursor', async () => {
  const { load, state, pending } = harness();
  const old = load();
  state.feed = 'problems';
  const latest = load();
  pending.get('missions/problems/feed?tab=open').resolve({ posts: [], topics: [], next: 'post-cursor' });
  await latest;
  pending.get('missions/feed?tab=popular').resolve({ missions: [], categories: [], next: 'mission-cursor' });
  await old;
  assert.equal(state.next, 'post-cursor');
});

test('search ignores results from an older query while the new query is waiting to run', async () => {
  const { search, state, pending, getElementById } = harness();
  state.q = 'old';
  const old = search();
  state.q = 'new';
  pending.get('missions/feed?tab=new&q=old').reject('Old query failed');
  pending.get('missions/people?q=old').resolve({ people: [] });
  pending.get('missions/problems/feed?tab=new&q=old').resolve({ posts: [] });
  await old;
  assert.equal(getElementById('discover-error').hidden, true);
});

test('a bridge rejection is readable and the closed side panel discards late failures', async () => {
  const { openProblem, closeSide, pending, getElementById } = harness();
  const current = openProblem('AB23CD45');
  pending.get('missions/problems/AB23CD45').reject('Sunucuya ulaşılamadı.');
  await current;
  const body = getElementById('discover-side-body');
  assert.equal(body.children[0].textContent, 'Sunucuya ulaşılamadı.');

  const closing = openProblem('CD23AB45');
  closeSide();
  pending.get('missions/problems/CD23AB45').reject('Geç gelen hata');
  await closing;
  assert.equal(body.children[0].textContent, 'Yükleniyor…');
  assert.equal(getElementById('discover-side').hidden, true);
});

test('a shared problem opens after startup navigation completes', async () => {
  let finishStartup;
  const ready = new Promise((done) => { finishStartup = done; });
  const { pending } = harness({ ready, link: { kind: 'problem', code: 'AB23CD45' } });
  await new Promise(setImmediate);
  assert.equal(pending.has('missions/problems/AB23CD45'), false);
  finishStartup();
  await new Promise(setImmediate);
  assert.equal(pending.has('missions/problems/AB23CD45'), true);
});

test('a blocked public image is retried once across matching elements', async () => {
  const imageData = 'data:image/jpeg;base64,/9j/';
  const { displayImage, mediaCalls } = harness({ imageData });
  const src = 'https://api.wiqo.invalid/missions/avatars/user.name';
  const avatar = {};
  const repeated = {};
  displayImage(avatar, src);
  displayImage(repeated, src);
  assert.equal(avatar.hidden, true);
  await Promise.all([avatar.onerror(), repeated.onerror()]);
  assert.deepEqual(mediaCalls, [src]);
  assert.equal(avatar.src, imageData);
  assert.equal(repeated.src, imageData);
  avatar.onload();
  assert.equal(avatar.hidden, false);
});

test('removing a cover keeps its gradient when the old picture arrives', () => {
  const { paintCover, coverImages } = harness();
  const cover = { style: {} };
  paintCover(cover, 'indigo', 'https://api.wiqo.invalid/missions/covers/0123456789_ABCDEFGHIJKLM');
  paintCover(cover, 'graphite', undefined);
  coverImages[0].onload();
  assert.equal(cover.style.background, 'linear-gradient(135deg, #5c6470d9, #5c6470)');
});
