'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const REPO_ROOT = path.resolve(__dirname, '..');
const FRONTEND_DIR = path.join(REPO_ROOT, 'frontend');
const INDEX_HTML_PATH = path.join(FRONTEND_DIR, 'index.html');
const JS_SCRIPTS = ['bridge.js', 'i18n.js', 'dashboard.js', 'launcher.js'];
const HANGUL_RE = /[\u1100-\u11FF\u3130-\u318F\uAC00-\uD7AF]/;

function walkFiles(dirPath, filterFn, out = []) {
  if (!fs.existsSync(dirPath)) return out;
  for (const entry of fs.readdirSync(dirPath, { withFileTypes: true })) {
    const full = path.join(dirPath, entry.name);
    if (entry.isDirectory()) {
      walkFiles(full, filterFn, out);
    } else if (filterFn(full)) {
      out.push(full);
    }
  }
  return out;
}

function runStaticChecks() {
  const html = fs.readFileSync(INDEX_HTML_PATH, 'utf8');
  const rawIds = [...html.matchAll(/\sid="([^"]+)"/g)].map((m) => m[1]);
  const counts = new Map();
  for (const id of rawIds) {
    counts.set(id, (counts.get(id) || 0) + 1);
  }
  const duplicateIds = [...counts.entries()]
    .filter(([, count]) => count > 1)
    .map(([id]) => id);
  assert.deepEqual(duplicateIds, [], `Duplicate HTML id(s): ${duplicateIds.join(', ')}`);

  const htmlIds = new Set(rawIds);
  const referencedIds = new Set();
  for (const file of JS_SCRIPTS) {
    const src = fs.readFileSync(path.join(FRONTEND_DIR, 'js', file), 'utf8');
    for (const m of src.matchAll(/\$\('([^']+)'\)/g)) {
      referencedIds.add(m[1]);
    }
    for (const m of src.matchAll(/document\.getElementById\('([^']+)'\)/g)) {
      referencedIds.add(m[1]);
    }
  }
  const missingIds = [...referencedIds].filter((id) => !htmlIds.has(id)).sort();
  assert.deepEqual(missingIds, [], `JS references missing HTML id(s): ${missingIds.join(', ')}`);

  const codeFiles = [
    ...walkFiles(path.join(REPO_ROOT, 'src'), (p) => p.endsWith('.rs')),
    INDEX_HTML_PATH,
    ...walkFiles(path.join(FRONTEND_DIR, 'js'), (p) => p.endsWith('.js')),
    ...walkFiles(path.join(FRONTEND_DIR, 'css'), (p) => p.endsWith('.css')),
    ...walkFiles(path.join(REPO_ROOT, 'scripts'), (p) => fs.statSync(p).isFile()),
  ];

  const hangulHits = [];
  for (const filePath of codeFiles) {
    const lines = fs.readFileSync(filePath, 'utf8').split(/\r?\n/);
    lines.forEach((line, idx) => {
      if (HANGUL_RE.test(line)) {
        hangulHits.push(`${path.relative(REPO_ROOT, filePath)}:${idx + 1}`);
      }
    });
  }
  assert.deepEqual(hangulHits, [], `Unexpected Hangul in code/scripts: ${hangulHits.join(', ')}`);

  return {
    htmlIdCount: rawIds.length,
    jsRefCount: referencedIds.size,
  };
}

function createMockElement(id = '', ownerDoc = null) {
  const classes = new Set();
  const listeners = new Map();
  const attrs = new Map();

  const el = {
    id,
    value: '',
    textContent: '',
    innerHTML: '',
    placeholder: '',
    title: '',
    checked: false,
    disabled: false,
    isConnected: true,
    dataset: {},
    style: {},
    options: [],
    files: [],
    className: '',
    classList: {
      add: (...names) => names.forEach((n) => classes.add(n)),
      remove: (...names) => names.forEach((n) => classes.delete(n)),
      toggle: (name, force) => {
        if (force === undefined) {
          if (classes.has(name)) {
            classes.delete(name);
            return false;
          }
          classes.add(name);
          return true;
        }
        if (force) classes.add(name);
        else classes.delete(name);
        return !!force;
      },
      contains: (name) => classes.has(name),
    },
    addEventListener: (type, handler) => {
      if (typeof handler !== 'function') return;
      if (!listeners.has(type)) listeners.set(type, new Set());
      listeners.get(type).add(handler);
    },
    removeEventListener: (type, handler) => {
      listeners.get(type)?.delete(handler);
    },
    dispatchEvent: (ev) => {
      const eventObj = ev && typeof ev === 'object' ? ev : { type: String(ev) };
      if (!eventObj.target) eventObj.target = el;
      if (!eventObj.preventDefault) eventObj.preventDefault = () => {};
      if (!eventObj.stopPropagation) eventObj.stopPropagation = () => {};
      const handlers = [...(listeners.get(eventObj.type) || [])];
      for (const fn of handlers) {
        fn(eventObj);
      }
    },
    click: () => {
      el.dispatchEvent({ type: 'click', target: el });
    },
    focus: () => {
      if (ownerDoc) ownerDoc.activeElement = el;
    },
    setAttribute: (k, v) => attrs.set(String(k), String(v)),
    removeAttribute: (k) => attrs.delete(String(k)),
    getAttribute: (k) => (attrs.has(String(k)) ? attrs.get(String(k)) : null),
    appendChild: () => {},
    remove: () => {},
    querySelector: () => null,
    querySelectorAll: () => [],
    closest: () => null,
  };
  return el;
}

function buildBootContext(htmlIds) {
  const elMap = new Map();
  const docListeners = new Map();
  const tauriListeners = new Map();
  const consoleErrors = [];

  const mockDocument = {
    documentElement: { lang: 'en' },
    activeElement: null,
    getElementById: (id) => elMap.get(id) || null,
    querySelector: () => null,
    querySelectorAll: () => [],
    createElement: (tag) => createMockElement('', mockDocument),
    addEventListener: (type, handler) => {
      if (typeof handler !== 'function') return;
      if (!docListeners.has(type)) docListeners.set(type, new Set());
      docListeners.get(type).add(handler);
    },
    removeEventListener: (type, handler) => {
      docListeners.get(type)?.delete(handler);
    },
  };

  for (const id of htmlIds) {
    const el = createMockElement(id, mockDocument);
    if (id === 'confirmModal') {
      el.classList.add('hidden');
    }
    elMap.set(id, el);
  }

  function emitTauri(eventName, payload) {
    const handlers = [...(tauriListeners.get(eventName) || [])];
    for (const fn of handlers) {
      fn({ event: eventName, payload });
    }
  }

  const activeEstateRow = {
    land_ident: 34000003,
    territory_type_id: 341,
    ward_index: 0,
    division: 0,
    plot_index: 0,
    room_number: 0,
    plot_size: 2,
    is_apartment: false,
    estate_name: 'Test House',
    owner_name: 'Foundation Test',
    greeting: '',
    light_level: 3,
    enable_ssao: true,
    visitors_allowed: true,
    interior_style: 'goblet',
    updated_at: 1000,
  };

  const mockTauri = {
    core: {
      invoke: async (cmd, args = {}) => {
        switch (cmd) {
          case 'get_settings':
            return {
              uiLocale: 'en',
              gameDataLocale: 'follow_client',
              language: 1,
              serverUrl: 'http://127.0.0.1:21062',
              gamePath: '',
              watchedPresetDirs: ['D:\\Save'],
              autoApplyWatchedPresets: false,
              alwaysOnTop: false,
              minimizeToTrayOnClose: true,
            };
          case 'get_system_locale_cmd':
            return 'en';
          case 'set_ui_locale_cmd':
            return args.locale === 'auto' ? 'en' : args.locale;
          case 'get_game_data_info_cmd':
            return {
              gameDirValid: false,
              requestedLocale: 'en',
              effectiveLocale: 'en',
              requestedInSqpack: false,
              usingKoDataDir: false,
              koDataDirValid: false,
              availableLanguages: [],
            };
          case 'scan_local_presets':
            return [
              {
                id: 'local-preset-1',
                path: 'D:\\Save\\sample.json',
                dir: 'D:\\Save',
                fileName: 'sample.json',
                relativePath: 'sample.json',
                modifiedUnixMs: 1000,
                meta: {
                  houseSize: 'Large',
                  district: 'The Goblet',
                  indoorCount: 10,
                  interiorDyed: 0,
                  interiorMaterial: 0,
                  outdoorCount: 0,
                  outdoorDyed: 0,
                  interiorFixtures: [],
                  exteriorFixtures: [],
                  interiorScale: 1.0,
                  exteriorScale: 1.0,
                },
              },
            ];
          case 'get_server_logs':
            return [];
          case 'get_server_status':
            return {
              mode: 'managed',
              pid: 43412,
              protocolPatch: '7.58',
              preflightIssues: [],
            };
          case 'get_setup_status_cmd':
            return {
              busy: false,
              foundationInstalled: true,
              foundationHasOodleFeature: true,
              oodleInstalled: true,
              foundationHasConfig: true,
              gamePathValid: true,
              remakeplaceValid: false,
              rmpBridgeInstalled: false,
              ue4ssInstalled: false,
            };
          case 'get_rmp_status':
            return { phase: 'idle', running: false, relay: {} };
          case 'check_runtime_updates_cmd':
            return {};
          case 'housing_reconnect_sse':
            queueMicrotask(() => {
              emitTauri('housing-sse-status', {
                connected: true,
                url: 'http://127.0.0.1:21064/api/v1/housing/events',
              });
            });
            return null;
          case 'housing_api_request': {
            const reqPath = String(args.path || '');
            if (reqPath === '/status') {
              return {
                ok: true,
                status: 200,
                body: JSON.stringify({
                  protocol_patch: '7.58',
                  online_sessions: [
                    {
                      character_id: 1,
                      character_name: 'Foundation Test',
                      zone_id: 1251,
                      intended_use: 'HousingIndoor',
                      active_estate: activeEstateRow,
                    },
                  ],
                }),
              };
            }
            if (reqPath === '/estates') {
              return {
                ok: true,
                status: 200,
                body: JSON.stringify([
                  {
                    row: activeEstateRow,
                    indoor_count: 10,
                    outdoor_count: 2,
                    snapshot_count: 0,
                    pattern_name: 'goblet',
                  },
                ]),
              };
            }
            if (reqPath === '/presets') {
              return { ok: true, status: 200, body: JSON.stringify({ files: [] }) };
            }
            if (reqPath.startsWith(`/estates/${activeEstateRow.land_ident}`)) {
              return {
                ok: true,
                status: 200,
                body: JSON.stringify({
                  row: activeEstateRow,
                  indoor_count: 10,
                  outdoor_count: 2,
                  snapshots: [],
                }),
              };
            }
            return { ok: true, status: 200, body: '{}' };
          }
          default:
            return null;
        }
      },
    },
    event: {
      listen: async (eventName, handler) => {
        if (!tauriListeners.has(eventName)) tauriListeners.set(eventName, new Set());
        tauriListeners.get(eventName).add(handler);
        return () => {
          tauriListeners.get(eventName)?.delete(handler);
        };
      },
    },
  };

  const sandbox = {
    console: {
      log: () => {},
      warn: () => {},
      error: (...args) => consoleErrors.push(args.map(String).join(' ')),
    },
    setTimeout,
    clearTimeout,
    setInterval: () => 0,
    clearInterval: () => {},
    Promise,
    URL,
    TextEncoder,
    TextDecoder,
    Intl,
    Date,
    Math,
    JSON,
    Number,
    String,
    Boolean,
    Array,
    Object,
    Set,
    Map,
    RegExp,
    Error,
    TypeError,
    encodeURIComponent,
    decodeURIComponent,
    parseInt,
    parseFloat,
    isFinite,
    isNaN,
    navigator: {
      language: 'en-US',
      clipboard: { writeText: async () => {} },
    },
    localStorage: {
      getItem: () => null,
      setItem: () => {},
      removeItem: () => {},
    },
    fetch: async (url) => {
      const m = String(url).match(/locales\/([a-z]+)\.json$/);
      if (!m) return { ok: false, status: 404 };
      const localePath = path.join(FRONTEND_DIR, 'locales', `${m[1]}.json`);
      const text = fs.readFileSync(localePath, 'utf8');
      return {
        ok: true,
        status: 200,
        json: async () => JSON.parse(text),
      };
    },
    document: mockDocument,
    __TAURI__: mockTauri,
  };
  sandbox.window = sandbox;
  vm.createContext(sandbox);

  return { sandbox, elMap, consoleErrors };
}

async function runBootSmokeTest() {
  const html = fs.readFileSync(INDEX_HTML_PATH, 'utf8');
  const htmlIds = new Set([...html.matchAll(/\sid="([^"]+)"/g)].map((m) => m[1]));
  const { sandbox, elMap, consoleErrors } = buildBootContext(htmlIds);

  for (const file of JS_SCRIPTS) {
    const filePath = path.join(FRONTEND_DIR, 'js', file);
    const code = fs.readFileSync(filePath, 'utf8');
    vm.runInContext(code, sandbox, { filename: file });
  }

  await new Promise((resolve) => setTimeout(resolve, 350));

  assert.deepEqual(
    consoleErrors,
    [],
    `Unexpected console.error during frontend boot: ${consoleErrors.join(' | ')}`
  );
  assert.ok(sandbox.window.FoundationBridge, 'FoundationBridge should be defined');
  assert.ok(sandbox.window.FoundationI18n, 'FoundationI18n should be defined');
  assert.ok(sandbox.window.FoundationStudioApp, 'FoundationStudioApp should be defined');

  const expectedExports = [
    'switchTab',
    'openEstateDetail',
    'refreshAll',
    'refreshLocalPresets',
    'toast',
    'confirmAction',
    'pushExternalLog',
    'syncWatchedSettings',
  ];
  for (const key of expectedExports) {
    assert.equal(
      typeof sandbox.window.FoundationStudioApp[key],
      'function',
      `FoundationStudioApp.${key} must be a function`
    );
  }

  const sessionText = elMap.get('sessionPlayerText').textContent;
  assert.ok(
    sessionText.includes('Foundation Test'),
    `Expected sessionPlayerText to include 'Foundation Test', got: ${sessionText}`
  );
  const homeCurrentText = elMap.get('homeCurrentText').textContent;
  assert.ok(
    homeCurrentText.includes('Test House'),
    `Expected homeCurrentText to include 'Test House', got: ${homeCurrentText}`
  );
  assert.equal(String(elMap.get('navCountEstates').textContent), '1', 'navCountEstates must equal 1');
  assert.equal(String(elMap.get('navCountPresets').textContent), '1', 'navCountPresets must equal 1');

  const confirmModal = elMap.get('confirmModal');
  const confirmOk = elMap.get('confirmModalOk');
  const confirmPromise = sandbox.window.FoundationStudioApp.confirmAction('Confirm Title', 'Confirm Body');
  assert.equal(
    confirmModal.classList.contains('hidden'),
    false,
    'confirmModal should be visible after confirmAction()'
  );
  assert.equal(elMap.get('confirmModalTitle').textContent, 'Confirm Title');
  assert.equal(elMap.get('confirmModalMessage').textContent, 'Confirm Body');
  confirmOk.click();
  const confirmed = await confirmPromise;
  assert.equal(confirmed, true, 'confirmAction() should resolve true when OK is clicked');
  assert.equal(
    confirmModal.classList.contains('hidden'),
    true,
    'confirmModal should be hidden again after OK is clicked'
  );
}

async function main() {
  const stats = runStaticChecks();
  await runBootSmokeTest();
  console.log(
    `smoke-frontend OK (htmlIds=${stats.htmlIdCount}, jsRefs=${stats.jsRefCount}, boot=ok, confirmAction=ok)`
  );
  process.exit(0);
}

main().catch((err) => {
  console.error(err && err.stack ? err.stack : err);
  process.exit(1);
});
