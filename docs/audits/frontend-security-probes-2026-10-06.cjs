// Historical audit probes for the unsafe baseline, not acceptance tests for a fix.
// On fixed code use `cd web && npm test`; these historical assertions should fail.
// Run from the repository root: node docs/audits/frontend-security-probes-2026-10-06.cjs
// Hooks/network are controlled; JSX is rendered by the installed React renderer.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { createRequire } = require('node:module');
const root = path.resolve(__dirname, '../..');
const webRequire = createRequire(path.join(root, 'web/package.json'));
const ts = webRequire('typescript');
const React = webRequire('react');
const { renderToStaticMarkup } = webRequire('react-dom/server');

function load(relativePath, mocks = {}) {
  const filename = path.join(root, 'web/src', relativePath);
  const source = ts.transpileModule(fs.readFileSync(filename, 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
  }).outputText;
  const module = { exports: {} };
  const req = (id) => Object.hasOwn(mocks, id) ? mocks[id] : webRequire(id);
  vm.runInNewContext(source, { module, exports: module.exports, require: req }, { filename });
  return module.exports;
}

async function logoutProbe() {
  const values = ['admin', false];
  let n = 0;
  const auth = load('auth.tsx', {
    react: {
      ...React,
      useState: () => {
        const i = n++;
        return [values[i], (v) => { values[i] = v; }];
      },
      useEffect: () => {},
    },
    './api': {
      api: async () => { throw new Error('request never reached server'); },
      setUnauthorizedHandler: () => {},
    },
  });
  const provider = auth.AuthProvider({ children: null });
  await assert.rejects(provider.props.value.logout(), /never reached server/);
  assert.equal(values[0], null);
  console.log('FE-02 reproduced: failed logout clears local user although no server request succeeded');
}

function nodesProbe() {
  const schema = load('components/nodeSchema.ts');
  const passthrough = ({ children }) => children;
  const mocks = {
    react: {
      ...React,
      useEffect: () => {},
      useCallback: (fn) => fn,
      useMemo: (fn) => fn(),
    },
    antd: { App: { useApp: () => ({ message: {} }) } },
    '@ant-design/icons': new Proxy({}, { get: () => () => null }),
    '@dnd-kit/core': {
      DndContext: passthrough, PointerSensor: {}, closestCenter: () => {},
      useSensor: () => {}, useSensors: () => [],
    },
    '@dnd-kit/sortable': {
      SortableContext: passthrough, arrayMove: () => {}, verticalListSortingStrategy: {},
      useSortable: () => ({ attributes: {}, listeners: {}, setNodeRef: () => {} }),
    },
    '@dnd-kit/utilities': { CSS: { Transform: { toString: () => undefined } } },
    'react-i18next': { useTranslation: () => ({ t: (key) => key }) },
    '../../api': {},
    '../../components/regenerate': { useRegenerateNotice: () => () => {} },
    '../../components/useSerialSave': { useSerialSave: () => () => {} },
    '../../components/nodeSchema': schema,
  };
  const { default: NodesCard } = load('pages/detail/NodesCard.tsx', mocks);
  function render(type, name = 'probe') {
    const states = [[{ name, type }], true, ['provider', 'custom']];
    let n = 0;
    mocks.react.useState = () => [states[n++], () => {}];
    return renderToStaticMarkup(NodesCard({
      profileId: 'probe', profileName: 'probe', nodes: [], generatedAt: null,
      onRegenerate: () => {},
    }));
  }
  assert.match(render('ss'), /Shadowsocks/);
  assert.throws(() => render('__proto__'), /Objects are not valid as a React child/);
  const html = render('ss', '<img src=x onerror=alert(1)>');
  assert.match(html, /&lt;img/);
  assert.doesNotMatch(html, /<img/);
  console.log('FE-01 reproduced: real NodesCard JSX crashes on provider type __proto__');
  console.log('XSS negative control passed: HTML in provider name is escaped');
}

function profileSwitchProbe() {
  // Simulate the retained component state during A -> B navigation, while B's
  // GET is still pending. No browser navigation or real data mutation occurs.
  const oldDetail = {
    id: 'A', name: 'Profile A', subscription_url: 'https://sub.example/A',
    source_url_masked: 'https://provider.example/***', nodes: [], groups: [],
    rules: null, last_generated_at: null,
  };
  const states = [oldDetail, false, false, [], []];
  let n = 0;
  const requests = [];
  const component = load('pages/ProfileDetail.tsx', {
    react: {
      ...React,
      useState: () => [states[n++], () => {}],
      useRef: () => ({ current: 0 }), useCallback: (fn) => fn, useEffect: () => {},
    },
    'react-router': { Link: 'Link', useParams: () => ({ id: 'B' }) },
    'react-i18next': { useTranslation: () => ({ t: (key) => key }) },
    antd: {
      App: { useApp: () => ({ message: { success: () => {} } }) },
      Button: 'Button', Form: 'Form', Input: 'Input', Modal: 'Modal',
      Popconfirm: 'Popconfirm', QRCode: 'QRCode', Spin: 'Spin', Tabs: 'Tabs',
    },
    '@ant-design/icons': new Proxy({}, { get: () => () => null }),
    '../api': {
      api: async (url) => { requests.push(url); return new Promise(() => {}); },
      ApiError: class extends Error {}, errorMessage: () => '',
    },
    './detail/NodesCard': { default: () => null, __esModule: true },
    './detail/GroupsCard': { default: () => null, __esModule: true },
    './detail/RulesCard': { default: () => null, __esModule: true },
    '../components/cards.css': {}, './detail/detail.css': {},
  });
  function find(element, type) {
    if (!element || typeof element !== 'object') return null;
    if (element.type === type) return element;
    for (const child of React.Children.toArray(element.props?.children)) {
      const match = find(child, type);
      if (match) return match;
    }
    return null;
  }
  const page = component.default();
  const basic = find(page, 'Tabs').props.items[0].children;
  assert.equal(basic.props.detail.id, 'A');
  void basic.props.onRefresh();
  assert.equal(requests[0], '/api/profiles/B/generate');
  console.log('FE-04 reproduced: visible detail is A while its refresh action targets route B');
}

logoutProbe().then(() => { nodesProbe(); profileSwitchProbe(); })
  .catch((error) => { console.error(error); process.exitCode = 1; });
