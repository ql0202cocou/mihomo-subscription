const assert = require('node:assert/strict');
const { test } = require('node:test');
const { load, hooks, find, deferred, tick, translation, icons, webRequire } = require('./helpers.cjs');
const { renderToStaticMarkup } = webRequire('react-dom/server');

test('provider and custom node types render safely, including inherited object keys', () => {
  const schema = load('components/nodeSchema.ts');
  const passthrough = ({ children }) => children;
  for (const type of ['ss', '__proto__', 'constructor', 'toString', 'new-protocol']) {
    const state = hooks([[{ name: '<img src=x onerror=alert(1)>', type }], true, ['provider', 'custom']]);
    const { default: NodesCard } = load('pages/detail/NodesCard.tsx', {
      react: state.react,
      antd: { App: { useApp: () => ({ message: {} }) } },
      '@ant-design/icons': icons,
      '@dnd-kit/core': {
        DndContext: passthrough, PointerSensor: {}, closestCenter: () => {},
        useSensor: () => {}, useSensors: () => [],
      },
      '@dnd-kit/sortable': {
        SortableContext: passthrough, arrayMove: () => {}, verticalListSortingStrategy: {},
        useSortable: () => ({ attributes: {}, listeners: {}, setNodeRef: () => {} }),
      },
      '@dnd-kit/utilities': { CSS: { Transform: { toString: () => undefined } } },
      'react-i18next': translation,
      '../../api': {},
      '../../components/regenerate': { useRegenerateNotice: () => () => {} },
      '../../components/useSerialSave': { useSerialSave: () => () => {} },
      '../../components/nodeSchema': schema,
    });
    const html = renderToStaticMarkup(state.render(NodesCard, {
      profileId: 'probe', profileName: 'probe', nodes: [{ name: 'custom', node_type: type }],
      generatedAt: null, onRegenerate: () => {},
    }));
    assert.ok(html.includes(type === 'ss' ? 'Shadowsocks' : type));
    assert.match(html, /&lt;img/);
    assert.doesNotMatch(html, /<img/);
  }
});

for (const status of ['network', 403, 500, 401, 204]) {
  test(`logout ${status}: only confirmed logout or expired session clears the user`, async () => {
    const state = hooks(['admin', false]);
    const requests = [];
    const api = load('api.ts', {}, { fetch: async (url, options) => {
      requests.push([url, options.method]);
      if (status === 'network') throw new Error('offline');
      return new Response(null, { status });
    } });
    const auth = load('auth.tsx', { react: state.react, './api': api });
    const value = () => state.render(auth.AuthProvider, { children: null }).props.value;
    if (status === 204 || status === 401) {
      await value().logout();
      assert.equal(value().user, null);
    } else {
      await assert.rejects(value().logout());
      assert.equal(value().user, 'admin');
    }
    assert.deepEqual(requests, [['/api/auth/logout', 'POST']]);
  });
}

for (const success of [false, true]) {
  test(`logout UI ${success ? 'success' : 'failure'} handles pending clicks and navigation`, async () => {
    const state = hooks();
    const request = deferred();
    const messages = [], navigations = [];
    let calls = 0;
    const { default: AppLayout } = load('components/AppLayout.tsx', {
      react: state.react,
      antd: { App: { useApp: () => ({ message: { error: (text) => messages.push(text) } }) } },
      'react-router': { Link: 'Link', Outlet: 'Outlet', useLocation: () => ({ pathname: '/' }),
        useNavigate: () => (...args) => navigations.push(args) },
      'react-i18next': translation,
      '@ant-design/icons': icons,
      '../auth': { useAuth: () => ({ user: 'admin', logout: () => { calls++; return request.promise; } }) },
      '../theme': { useTheme: () => ({ mode: 'light', toggle: () => {} }) },
      './PageErrorBoundary': { default: 'PageErrorBoundary', __esModule: true },
    });
    const button = () => find(state.render(AppLayout), (e) => e.props?.className === 'account-logout');
    const click = button().props.onClick;
    const first = click();
    await click();
    assert.equal(calls, 1);
    assert.equal(button().props.disabled, true);
    assert.equal(navigations.length, 0);
    if (success) request.resolve();
    else request.reject(new Error('offline'));
    await first;
    assert.equal(button().props.disabled, false);
    if (success) {
      assert.equal(navigations[0][0], '/login');
      assert.equal(messages.length, 0);
    } else {
      assert.equal(navigations.length, 0);
      assert.deepEqual(messages, ['nav.logoutFailed']);
    }
  });
}

function detailModule(state, getId, api, messages = []) {
  return load('pages/ProfileDetail.tsx', {
    react: state.react,
    'react-router': { Link: 'Link', useParams: () => ({ id: getId() }) },
    'react-i18next': translation,
    antd: { App: { useApp: () => ({ message: {
      success: (text) => messages.push(text), error: (text) => messages.push(text),
    } }) }, Button: 'Button', Form: 'Form', Input: 'Input', Modal: 'Modal',
    Popconfirm: 'Popconfirm', QRCode: 'QRCode', Spin: 'Spin', Switch: 'Switch', Tabs: 'Tabs' },
    '@ant-design/icons': icons,
    '../api': { api, ApiError: class extends Error {}, errorMessage: () => 'failed' },
    '../components/regenerate': { useRegenerateNotice: () => () => {} },
    './detail/NodesCard': { default: 'NodesCard', __esModule: true },
    './detail/GroupsCard': { default: 'GroupsCard', __esModule: true },
    './detail/RulesCard': { default: 'RulesCard', __esModule: true },
  });
}

const detail = (id, name = id) => ({ id, name, nodes: [], groups: [], rules: null,
  subscription_url: `https://sub.example/${id}`, last_generated_at: null,
  reality_mlkem: true, reality_fingerprint_risks: [] });
const tabs = (element) => find(element, (e) => e.type === 'Tabs');

test('switching A to B resets detail immediately; a late A response cannot expose old actions', async () => {
  let id = 'A';
  const stateA = hooks();
  const requestA = deferred();
  const wrapper = detailModule(stateA, () => id, () => requestA.promise).default;
  const elementA = wrapper();
  stateA.render(elementA.type, elementA.props);
  stateA.effects();
  id = 'B';
  const elementB = wrapper();
  assert.notEqual(elementA.key, elementB.key);
  stateA.unmount();
  requestA.resolve(detail('A'));
  await tick();
  assert.equal(stateA.render(elementA.type, elementA.props), null);

  const stateB = hooks();
  const requestB = deferred();
  const moduleB = detailModule(stateB, () => id, () => requestB.promise);
  const pageB = moduleB.default();
  assert.equal(stateB.render(pageB.type, pageB.props), null);
  stateB.effects();
  requestB.reject(new Error('unavailable'));
  await tick();
  const error = stateB.render(pageB.type, pageB.props);
  assert.equal(tabs(error), null);
  assert.match(renderToStaticMarkup(error), /common.loadFailed/);
});

test('detail reloads accept the newest result and an unmounted generation is ignored', async () => {
  const state = hooks();
  const requests = [];
  const messages = [];
  const module = detailModule(state, () => 'B', (url) => {
    const request = deferred(); requests.push({ url, ...request }); return request.promise;
  }, messages);
  const element = module.default();
  const render = () => state.render(element.type, element.props);
  render(); state.effects();
  requests[0].resolve(detail('B'));
  await tick();
  let basic = tabs(render()).props.items[0].children;
  const firstReload = basic.props.onSaved();
  const latestReload = basic.props.onSaved();
  requests[2].resolve(detail('B', 'newest'));
  await latestReload;
  requests[1].resolve(detail('B', 'stale'));
  await firstReload;
  basic = tabs(render()).props.items[0].children;
  assert.equal(basic.props.detail.name, 'newest');
  const generate = basic.props.onRefresh();
  assert.equal(requests[3].url, '/api/profiles/B/generate');
  state.unmount();
  requests[3].resolve({});
  await generate;
  assert.equal(requests.length, 4); // No obsolete reload after navigating away.
  assert.equal(messages.length, 0);
});

function rulesCardModule(state, api) {
  const passthrough = ({ children }) => children;
  const arrayMove = (arr, from, to) => {
    const next = arr.slice();
    const [moved] = next.splice(from, 1);
    next.splice(to, 0, moved);
    return next;
  };
  return load('pages/detail/RulesCard.tsx', {
    react: state.react,
    antd: {
      App: { useApp: () => ({ message: { info: () => {}, success: () => {}, error: () => {} } }) },
      AutoComplete: 'AutoComplete', Button: 'Button', Checkbox: 'Checkbox',
      Input: Object.assign(() => null, { TextArea: () => null }),
      InputNumber: 'InputNumber', Modal: 'Modal', Popconfirm: 'Popconfirm',
      Segmented: 'Segmented', Select: 'Select', Switch: 'Switch',
      Typography: { Text: 'Text' },
    },
    '@ant-design/icons': icons,
    '@dnd-kit/core': {
      DndContext: passthrough, PointerSensor: {}, closestCenter: () => {},
      useSensor: () => {}, useSensors: () => [],
    },
    '@dnd-kit/sortable': {
      SortableContext: passthrough, arrayMove, verticalListSortingStrategy: {},
      useSortable: () => ({ attributes: {}, listeners: {}, setNodeRef: () => {} }),
    },
    '@dnd-kit/utilities': { CSS: { Transform: { toString: () => undefined } } },
    'react-i18next': translation,
    '../../api': { api, errorMessage: () => 'failed' },
    '../../components/regenerate': { useRegenerateNotice: () => () => {} },
    '../../components/useSerialSave': { useSerialSave: () => async (run) => {
      try { return { ok: true, value: await run(), latest: true }; }
      catch (error) { return { ok: false, error, latest: true }; }
    } },
    './groupSchema': { BUILTIN_POLICIES: ['DIRECT', 'REJECT'] },
    '../../components/fields': { TypeChips: 'TypeChips' },
    '../../components/ruleSetConstants': {
      RULE_SET_BEHAVIORS: [], RULE_SET_MANUAL_FORMATS: [],
      RULE_SET_REMOTE_FORMATS: [], RULE_SET_SOURCES: [],
    },
  });
}

// 「导入机场规则」请求在途期间用户继续编辑(删除/重排):导入完成时必须合并在最新状态上,
// 不能回滚在途编辑。返回各辅助函数;puts 按顺序记录每次 PUT 提交完整规则文本。
function rulesCardSession(initial) {
  const state = hooks();
  const puts = [];
  const providerRules = deferred();
  const api = async (url, options = {}) => {
    if ((options.method ?? 'GET') === 'PUT') {
      puts.push(JSON.parse(options.body).content);
      return { regenerate: { status: 'applied' } };
    }
    if (url.endsWith('/provider-rules')) return providerRules.promise;
    if (url.endsWith('/proxies')) return { proxies: [], groups: [] };
    return [];
  };
  const { default: RulesCard } = rulesCardModule(state, api);
  const props = {
    profileId: 'p', initial, nodes: [], groups: [], generatedAt: null, errors: [],
    onSaved: () => {}, onRegenerate: () => {},
  };
  const render = () => state.render(RulesCard, props);
  return {
    puts, providerRules,
    async mount() {
      render(); state.effects();
      await tick();
      render(); state.effects();
    },
    render() { const tree = render(); state.effects(); return tree; },
    startImport(tree) {
      return find(tree, (e) => e.props?.title === 'rules.importConfirm').props.onConfirm();
    },
  };
}

test('importing provider rules merges onto a deletion made while the request is in flight', async () => {
  const session = rulesCardSession('DOMAIN,keep.example,DIRECT\nMATCH,DIRECT');
  await session.mount();

  // 发起「导入机场规则」:GET 挂起。
  let tree = session.render();
  const importing = session.startImport(tree);
  await tick();

  // 请求在途期间,用户删掉唯一的自定义规则。
  tree = session.render();
  find(tree, (e) => e.props?.line === 'DOMAIN,keep.example,DIRECT' && e.props?.onRemove)
    .props.onRemove();
  session.render();
  assert.deepEqual(session.puts, ['MATCH,DIRECT']);

  // 机场规则到达:应合并在删除后的最新状态上,不恢复被删的行。
  session.providerRules.resolve({ rules: ['DOMAIN,incoming.example,DIRECT'] });
  await importing;
  session.render();
  assert.deepEqual(session.puts,
    ['MATCH,DIRECT', 'DOMAIN,incoming.example,DIRECT\nMATCH,DIRECT']);
});

test('importing provider rules merges onto a reorder made while the request is in flight', async () => {
  const session = rulesCardSession('DOMAIN,a.example,DIRECT\nDOMAIN,b.example,DIRECT\nMATCH,DIRECT');
  await session.mount();

  let tree = session.render();
  const importing = session.startImport(tree);
  await tick();

  // 请求在途期间,用户拖拽把 b 排到 a 前。
  tree = session.render();
  find(tree, (e) => e.props?.onDragEnd).props.onDragEnd({
    active: { id: 'DOMAIN,b.example,DIRECT' },
    over: { id: 'DOMAIN,a.example,DIRECT' },
  });
  session.render();
  assert.deepEqual(session.puts,
    ['DOMAIN,b.example,DIRECT\nDOMAIN,a.example,DIRECT\nMATCH,DIRECT']);

  // 机场规则到达:追加在新顺序之后,不回滚重排。
  session.providerRules.resolve({ rules: ['DOMAIN,incoming.example,DIRECT'] });
  await importing;
  session.render();
  assert.deepEqual(session.puts, [
    'DOMAIN,b.example,DIRECT\nDOMAIN,a.example,DIRECT\nMATCH,DIRECT',
    'DOMAIN,b.example,DIRECT\nDOMAIN,a.example,DIRECT\nDOMAIN,incoming.example,DIRECT\nMATCH,DIRECT',
  ]);
});
