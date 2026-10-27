const assert = require('node:assert/strict');
const { test } = require('node:test');
const {
  load, hooks, find, findAll, deferred, tick, translation, icons, passthrough, dndMocks,
} = require('./helpers.cjs');

// 页面层行为:提交防重、加载失败与重试、陈旧读取失效、登录错误映射、会话探活、
// 主题存储兜底、请求头合并与排序传感器注册。

const messageLog = () => {
  const log = [];
  return {
    log,
    success: (text) => log.push(['success', text]),
    error: (text) => log.push(['error', text]),
    warning: (text) => log.push(['warning', text]),
    info: (text) => log.push(['info', text]),
  };
};

const serialImmediate = {
  useSerialSave: () => async (run) => {
    try { return { ok: true, value: await run(), latest: true }; }
    catch (error) { return { ok: false, error, latest: true }; }
  },
};

// ─── Settings:重置公共路径 ───────────────────────────────────────────────────

test('settings reset sends one request under a double confirm and shows progress', async () => {
  const state = hooks();
  const posts = [];
  let gate;
  const api = async (url) => {
    if (url.endsWith('/reset-public-path')) {
      posts.push(url);
      return new Promise((res) => { gate = () => res({ public_path_prefix: 'def' }); });
    }
    return { public_path_prefix: 'abc' };
  };
  const { default: Settings } = load('pages/Settings.tsx', {
    react: state.react,
    antd: {
      App: { useApp: () => ({ message: messageLog() }) },
      Button: 'Button', Input: 'Input', Modal: 'Modal', Segmented: 'Segmented',
    },
    'react-i18next': translation,
    '../api': { api, errorMessage: () => 'failed' },
    '../theme': { useTheme: () => ({ mode: 'light', setMode: () => {} }) },
  });
  let tree = state.render(Settings);
  state.effects();
  await tick();
  tree = state.render(Settings);
  const modal = () => find(tree, (e) => e.type === 'Modal');
  const first = modal().props.onOk();
  modal().props.onOk();
  await tick();
  assert.equal(posts.length, 1);
  tree = state.render(Settings);
  assert.equal(modal().props.confirmLoading, true);
  gate();
  await first;
  tree = state.render(Settings);
  assert.equal(modal().props.confirmLoading, false);
  assert.equal(posts.length, 1);
});

// ─── ProfileList:加载失败与新建防重 ──────────────────────────────────────────

function profileListModule(state, api) {
  return load('pages/ProfileList.tsx', {
    react: state.react,
    'react-router': { Link: 'Link' },
    antd: {
      App: { useApp: () => ({ message: messageLog() }) },
      Button: 'Button',
      Form: Object.assign(function Form() {}, {
        Item: function FormItem() {},
        useForm: () => [{ resetFields() {}, submit() {} }],
      }),
      Input: 'Input',
      Modal: 'Modal',
    },
    '@ant-design/icons': icons,
    'react-i18next': translation,
    '../api': { api, errorMessage: () => 'failed' },
  });
}

test('profile list shows retry on initial load failure instead of an empty library', async () => {
  const state = hooks();
  let fail = true;
  const api = async () => {
    if (fail) throw new Error('down');
    return [];
  };
  const { default: ProfileList } = profileListModule(state, api);
  state.render(ProfileList);
  state.effects();
  await tick();
  let tree = state.render(ProfileList);
  const retry = find(tree, (e) => e.type === 'Button' && e.props?.children === 'common.retry');
  assert.ok(retry);
  assert.equal(find(tree, (e) => e.props?.children === 'profiles.empty'), null);

  fail = false;
  retry.props.onClick();
  await tick();
  await tick();
  tree = state.render(ProfileList);
  assert.ok(find(tree, (e) => e.props?.children === 'profiles.empty'));
});

test('profile create submits once under a double confirm', async () => {
  const state = hooks();
  const posts = [];
  let gate;
  const api = async (url, options = {}) => {
    if ((options.method ?? 'GET') === 'POST') {
      posts.push(JSON.parse(options.body));
      return new Promise((res) => { gate = () => res({ id: 'p1' }); });
    }
    return [];
  };
  const { default: ProfileList } = profileListModule(state, api);
  let tree = state.render(ProfileList);
  state.effects();
  await tick();
  tree = state.render(ProfileList);
  const form = find(tree, (e) => e.props?.onFinish);
  const first = form.props.onFinish({ name: 'n', source_url: 'https://a.example' });
  form.props.onFinish({ name: 'n', source_url: 'https://a.example' });
  await tick();
  assert.equal(posts.length, 1);
  tree = state.render(ProfileList);
  assert.equal(find(tree, (e) => e.type === 'Modal').props.confirmLoading, true);
  gate();
  await first;
  tree = state.render(ProfileList);
  assert.equal(find(tree, (e) => e.type === 'Modal').props.confirmLoading, false);
  assert.equal(posts.length, 1);
});

// ─── GlobalNodes:加载失败重试、刷新失败保留列表、陈旧读取失效 ────────────────

function globalNodesModule(state, api, sensors) {
  return load('pages/GlobalNodes.tsx', {
    react: state.react,
    antd: {
      App: { useApp: () => ({ message: messageLog() }) },
      Button: 'Button', Modal: 'Modal', Popconfirm: 'Popconfirm',
    },
    '@ant-design/icons': icons,
    ...dndMocks(sensors),
    'react-i18next': translation,
    '../api': { api, errorMessage: () => 'failed' },
    '../components/useSerialSave': serialImmediate,
    '../components/NodeForm': {
      default: 'NodeForm', __esModule: true,
      contentToModel: () => ({}), modelToContent: () => '',
    },
    '../components/nodeSchema': { nodeTypeLabel: (s) => s },
  });
}

const nodeRowOrder = (tree) =>
  findAll(tree, (e) => e.props?.node?.name).map((e) => e.props.node.name);
const node = (name) => ({ id: name, name, node_type: 'ss', content: '', enabled: true });

test('global nodes offer retry after load failure and keep the list on refresh failure', async () => {
  const state = hooks();
  const sensors = [];
  let fail = true;
  const api = async (url, options = {}) => {
    if ((options.method ?? 'GET') === 'PUT') throw new Error('down');
    if (fail) throw new Error('down');
    return [node('a'), node('b')];
  };
  const { default: GlobalNodes } = globalNodesModule(state, api, sensors);
  state.render(GlobalNodes);
  state.effects();
  await tick();
  let tree = state.render(GlobalNodes);
  // 初次失败:错误 + 重试,而不是空列表文案
  const retry = find(tree, (e) => e.type === 'Button' && e.props?.children === 'common.retry');
  assert.ok(retry);
  assert.equal(find(tree, (e) => e.props?.children === 'nodes.customEmpty'), null);
  assert.ok(sensors.includes('KeyboardSensor') && sensors.includes('PointerSensor'));

  fail = false;
  retry.props.onClick();
  await tick();
  await tick();
  tree = state.render(GlobalNodes);
  assert.deepEqual(nodeRowOrder(tree), ['a', 'b']);

  // 刷新失败(排序保存后的回源):保留当前列表并显示失败提示
  fail = true;
  find(tree, (e) => e.props?.onDragEnd).props.onDragEnd({
    active: { id: 'a' }, over: { id: 'b' },
  });
  await tick();
  await tick();
  tree = state.render(GlobalNodes);
  assert.deepEqual(nodeRowOrder(tree), ['b', 'a']); // 列表不被清空
  assert.ok(find(tree, (e) => e.type === 'Button' && e.props?.children === 'common.retry'));
});

test('a load started before a drag cannot overwrite the newer order', async () => {
  const state = hooks();
  const sensors = [];
  const gets = [];
  const api = async (url, options = {}) => {
    if ((options.method ?? 'GET') === 'PUT') return {};
    const d = deferred();
    gets.push(d);
    return d.promise;
  };
  const { default: GlobalNodes } = globalNodesModule(state, api, sensors);
  let tree = state.render(GlobalNodes);
  state.effects();
  gets[0].resolve([node('a'), node('b')]);
  await tick();
  tree = state.render(GlobalNodes);
  assert.deepEqual(nodeRowOrder(tree), ['a', 'b']);

  // 第一次拖拽:乐观更新为 [b,a],随后触发的回源读取挂起。
  find(tree, (e) => e.props?.onDragEnd).props.onDragEnd({ active: { id: 'a' }, over: { id: 'b' } });
  await tick();
  await tick();
  tree = state.render(GlobalNodes);
  assert.deepEqual(nodeRowOrder(tree), ['b', 'a']);

  // 第一次读取仍在途时第二次拖拽:乐观更新为 [a,b],并再发一次读取。
  find(tree, (e) => e.props?.onDragEnd).props.onDragEnd({ active: { id: 'b' }, over: { id: 'a' } });
  await tick();
  await tick();
  tree = state.render(GlobalNodes);
  assert.deepEqual(nodeRowOrder(tree), ['a', 'b']);

  // 排序前发起的旧读取随后返回:结果被丢弃,不覆盖新顺序。
  gets[1].resolve([node('b'), node('a')]);
  await tick();
  tree = state.render(GlobalNodes);
  assert.deepEqual(nodeRowOrder(tree), ['a', 'b']);

  gets[2].resolve([node('a'), node('b')]);
  await tick();
  tree = state.render(GlobalNodes);
  assert.deepEqual(nodeRowOrder(tree), ['a', 'b']);
});

// ─── RuleSets:重试与保存防重 ─────────────────────────────────────────────────

test('rule sets retry after load failure and save once under a double confirm', async () => {
  const state = hooks();
  const sensors = [];
  const posts = [];
  let fail = true;
  let gate;
  const api = async (url, options = {}) => {
    if ((options.method ?? 'GET') === 'POST') {
      posts.push(JSON.parse(options.body));
      return new Promise((res) => { gate = () => res({}); });
    }
    if (fail) throw new Error('down');
    return [];
  };
  const { default: RuleSets } = load('pages/RuleSets.tsx', {
    react: state.react,
    antd: {
      App: { useApp: () => ({ message: messageLog() }) },
      Button: 'Button', Input: 'Input', InputNumber: 'InputNumber',
      Modal: 'Modal', Popconfirm: 'Popconfirm', Switch: 'Switch',
    },
    '@ant-design/icons': icons,
    ...dndMocks(sensors),
    'react-i18next': translation,
    '../api': { api, errorMessage: () => 'failed' },
    '../components/useSerialSave': serialImmediate,
    '../components/fields': { TypeChips: 'TypeChips' },
  });
  let tree = state.render(RuleSets);
  state.effects();
  await tick();
  tree = state.render(RuleSets);
  const retry = find(tree, (e) => e.type === 'Button' && e.props?.children === 'common.retry');
  assert.ok(retry);
  assert.ok(sensors.includes('KeyboardSensor') && sensors.includes('PointerSensor'));

  fail = false;
  retry.props.onClick();
  await tick();
  await tick();
  tree = state.render(RuleSets);

  find(tree, (e) => e.type === 'Button' && e.props?.children === 'ruleSets.add').props.onClick();
  tree = state.render(RuleSets);
  find(tree, (e) => e.props?.placeholder === 'ruleSets.namePlaceholder')
    .props.onChange({ target: { value: 'ads' } });
  tree = state.render(RuleSets);
  const modal = () => find(tree, (e) => e.type === 'Modal');
  const first = modal().props.onOk();
  modal().props.onOk();
  await tick();
  assert.equal(posts.length, 1);
  tree = state.render(RuleSets);
  assert.equal(modal().props.confirmLoading, true);
  gate();
  await first;
  tree = state.render(RuleSets);
  assert.equal(modal().props.confirmLoading, false);
  assert.equal(posts.length, 1);
  assert.equal(posts[0].name, 'ads');
});

// ─── NodesCard:传感器注册 ────────────────────────────────────────────────────

test('node section list registers keyboard and pointer sensors', () => {
  const state = hooks();
  const sensors = [];
  const { default: NodesCard } = load('pages/detail/NodesCard.tsx', {
    react: state.react,
    antd: { App: { useApp: () => ({ message: messageLog() }) } },
    '@ant-design/icons': icons,
    ...dndMocks(sensors),
    'react-i18next': translation,
    '../../api': { api: async () => ({ proxies: [], groups: [], generated: true, node_section_order: ['provider', 'custom'] }) },
    '../../components/nodeSchema': { nodeTypeLabel: (s) => s },
    '../../components/regenerate': { useRegenerateNotice: () => () => {} },
    '../../components/useSerialSave': serialImmediate,
  });
  state.render(NodesCard, {
    profileId: 'p', profileName: 'p', nodes: [], generatedAt: null, onRegenerate: () => {},
  });
  assert.ok(sensors.includes('KeyboardSensor'));
  assert.ok(sensors.includes('PointerSensor'));
});

// ─── Login:错误映射 ──────────────────────────────────────────────────────────

for (const [outcome, expected] of [
  [401, 'login.failed'],
  [429, 'login.tooMany'],
  [500, 'login.serviceError'],
  ['network', 'login.networkError'],
]) {
  test(`login shows ${expected} when sign-in fails with ${outcome}`, async () => {
    const state = hooks();
    class ApiError extends Error {
      constructor(status) { super(`HTTP ${status}`); this.status = status; }
    }
    const err = outcome === 'network' ? new Error('offline') : new ApiError(outcome);
    const navigations = [];
    const { default: Login } = load('pages/Login.tsx', {
      react: state.react,
      'react-router': {
        useLocation: () => ({ state: null }),
        useNavigate: () => (...args) => navigations.push(args),
      },
      antd: {
        Button: 'Button',
        Input: Object.assign(function Input() {}, { Password: function Password() {} }),
      },
      '@ant-design/icons': icons,
      'react-i18next': translation,
      '../auth': { useAuth: () => ({ login: async () => { throw err; } }) },
      '../api': { ApiError },
    });
    const tree = state.render(Login);
    await find(tree, (e) => e.type === 'form').props.onSubmit({ preventDefault() {} });
    const banner = find(state.render(Login), (e) => e.props?.className === 'login-error');
    assert.equal(find(banner, (e) => e.type === 'span').props.children, expected);
    assert.equal(navigations.length, 0);
  });
}

// ─── auth:会话探活与登录后的严格确认 ─────────────────────────────────────────

test('startup session probe tolerates failure and settles as signed out', async () => {
  const state = hooks();
  const auth = load('auth.tsx', {
    react: state.react,
    './api': {
      api: async () => { throw new Error('offline'); },
      ApiError: class extends Error {},
      setUnauthorizedHandler() {},
    },
  });
  state.render(auth.AuthProvider, { children: null });
  state.effects();
  await tick();
  const value = state.render(auth.AuthProvider, { children: null }).props.value;
  assert.equal(value.user, null);
  assert.equal(value.loading, false);
});

for (const status of [500, 401]) {
  test(`post-login session check ${status}: ${status === 401 ? 'settles signed out' : 'rejects the login'}`, async () => {
    const state = hooks();
    class ApiError extends Error {
      constructor(s) { super(`HTTP ${s}`); this.status = s; }
    }
    const auth = load('auth.tsx', {
      react: state.react,
      './api': {
        api: async (url) => {
          if (url.endsWith('/login')) return {};
          throw new ApiError(status);
        },
        ApiError,
        setUnauthorizedHandler() {},
      },
    });
    const value = () => state.render(auth.AuthProvider, { children: null }).props.value;
    if (status === 401) {
      await value().login('a', 'b');
      assert.equal(value().user, null);
    } else {
      await assert.rejects(value().login('a', 'b'));
      assert.equal(value().user, null);
    }
  });
}

// ─── theme:存储兜底与绘制前生效 ──────────────────────────────────────────────

test('theme applies dataset during render and tolerates unavailable storage', () => {
  const state = hooks();
  const documentElement = { dataset: {} };
  const denied = {
    getItem() { throw new Error('denied'); },
    setItem() { throw new Error('denied'); },
  };
  const theme = load('theme.tsx', {
    react: state.react,
    antd: {
      App: passthrough,
      ConfigProvider: passthrough,
      theme: { darkAlgorithm: {}, defaultAlgorithm: {} },
    },
  }, { localStorage: denied, document: { documentElement } });
  let el = state.render(theme.ThemeProvider, { children: null });
  // 布局副作用随 render 同步冲刷:不等待被动 effect 即已设置 dataset
  assert.equal(documentElement.dataset.theme, 'light'); // 读取失败退回亮色
  el.props.value.setMode('dark');
  el = state.render(theme.ThemeProvider, { children: null });
  assert.equal(documentElement.dataset.theme, 'dark'); // 写入失败也不影响当前会话
});

// ─── api:请求头合并 ──────────────────────────────────────────────────────────

test('api merges the default content type with caller headers, caller wins on conflicts', async () => {
  const seen = [];
  const api = load('api.ts', {}, {
    fetch: async (url, init) => {
      seen.push(init);
      return new Response('{}', { status: 200, headers: { 'content-type': 'application/json' } });
    },
  });
  await api.api('/x');
  assert.equal(seen[0].headers['Content-Type'], 'application/json');
  assert.equal(Object.keys(seen[0].headers).length, 1);
  await api.api('/x', { method: 'POST', headers: { 'X-Custom': '1' } });
  assert.equal(seen[1].headers['Content-Type'], 'application/json');
  assert.equal(seen[1].headers['X-Custom'], '1');
  assert.equal(seen[1].method, 'POST');
  await api.api('/x', { headers: { 'Content-Type': 'text/plain' } });
  assert.equal(seen[2].headers['Content-Type'], 'text/plain');
});

// ─── useSerialSave:泛型箭头经真实文件名转译可用 ─────────────────────────────

test('serial save runs in order and flags only the last save as latest', async () => {
  const state = hooks();
  const { useSerialSave } = load('components/useSerialSave.ts', { react: state.react });
  const save = state.render(() => useSerialSave());
  const order = [];
  const first = deferred();
  const r1 = save(async () => { await first.promise; order.push(1); return 'a'; });
  const r2 = save(async () => { order.push(2); return 'b'; });
  first.resolve();
  const [v1, v2] = await Promise.all([r1, r2]);
  assert.deepEqual(order, [1, 2]);
  assert.equal(v1.latest, false);
  assert.equal(v2.latest, true);
});

// ─── ProfileDetail:reset-token 防重/pending 提示,onRegenerate 清理 ───────────

// 模块内 instanceof 判断与测试抛出必须使用同一个类。
class FakeApiError extends Error {
  constructor(status, body) {
    super(`HTTP ${status}`);
    this.status = status;
    this.details = body?.details;
  }
}

function detailModule(state, api, messages) {
  return load('pages/ProfileDetail.tsx', {
    react: state.react,
    'react-router': { Link: 'Link', useParams: () => ({ id: 'B' }) },
    'react-i18next': translation,
    antd: {
      App: { useApp: () => ({ message: messages }) },
      Button: 'Button',
      Form: Object.assign(function Form() {}, {
        Item: function FormItem() {},
        useForm: () => [{ setFieldsValue() {} }],
      }),
      Input: 'Input', Modal: 'Modal', Popconfirm: 'Popconfirm',
      QRCode: 'QRCode', Spin: 'Spin', Switch: 'Switch', Tabs: 'Tabs',
    },
    '@ant-design/icons': icons,
    '../api': { api, ApiError: FakeApiError, errorMessage: () => 'failed' },
    '../components/regenerate': { useRegenerateNotice: () => () => {} },
    './detail/NodesCard': { default: 'NodesCard', __esModule: true },
    './detail/GroupsCard': { default: 'GroupsCard', __esModule: true },
    './detail/RulesCard': { default: 'RulesCard', __esModule: true },
  });
}

const detail = () => ({
  id: 'B', name: 'B', nodes: [], groups: [], rules: null,
  subscription_url: 'https://sub.example/B', last_generated_at: null,
  reality_mlkem: true, reality_fingerprint_risks: [],
});

function mountDetail(state, module) {
  const el = module.default();
  const render = () => state.render(el.type, el.props);
  render();
  state.effects();
  return render;
}

// 内部组件由 harness 按真实 hook 顺序逐层展开。
const expandHostedLink = (tree) => {
  const el = find(tree, (e) => typeof e.type === 'function' && e.props?.detail && e.props?.onReset);
  return el.type(el.props);
};
const expandPreview = (hosted) => {
  const el = find(hosted, (e) => typeof e.type === 'function' && e.props?.profileId && e.props?.onClose);
  return el.type(el.props);
};

test('hosted QR code keeps a dark foreground on its white frame', async () => {
  const state = hooks();
  const render = mountDetail(state, detailModule(state, async () => detail(), messageLog()));
  await tick();
  const qrCode = find(expandHostedLink(render()), (e) => e.type === 'QRCode');
  assert.equal(qrCode.props.value, detail().subscription_url);
  assert.equal(qrCode.props.color, '#000');
});

test('output preview follows the copy action and loads YAML in a modal', async () => {
  const state = hooks();
  const requests = [];
  const output = 'proxies:\n  - name: node-a';
  const api = async (url) => {
    requests.push(url);
    return url.endsWith('/preview') ? output : detail();
  };
  const render = mountDetail(state, detailModule(state, api, messageLog()));
  await tick();
  let tree = render();
  const tabKeys = find(tree, (e) => e.type === 'Tabs').props.items.map((item) => item.key);
  assert.equal(tabKeys.join(','), 'basic,nodes,groups,rules');

  let hosted = expandHostedLink(tree);
  const actions = findAll(hosted, (e) => e.type === 'Button').map((e) => e.props.children);
  assert.deepEqual(actions, ['detail.copy', 'preview.title', 'detail.resetToken']);
  find(hosted, (e) => e.type === 'Button' && e.props.children === 'preview.title').props.onClick();

  hosted = expandHostedLink(render());
  expandPreview(hosted);
  state.effects();
  await tick();
  assert.deepEqual(requests.filter((url) => url.endsWith('/preview')), ['/api/profiles/B/preview']);

  const preview = expandPreview(expandHostedLink(render()));
  const modal = find(preview, (e) => e.type === 'Modal');
  assert.equal(modal.props.title, 'preview.title');
  assert.equal(modal.props.open, true);
  assert.equal(find(preview, (e) => e.props?.className === 'preview-pre').props.children, output);
});

test('reset-token confirms once and reports a pending result distinctly', async () => {
  const state = hooks();
  const messages = messageLog();
  const resets = [];
  let gate;
  const api = async (url, options = {}) => {
    if (url.endsWith('/reset-token')) {
      resets.push(url);
      return new Promise((res) => { gate = () => res({ regenerate: { status: 'pending' } }); });
    }
    return detail();
  };
  const render = mountDetail(state, detailModule(state, api, messages));
  await tick();
  const confirmIn = (hosted) => find(hosted, (e) => e.props?.title === 'detail.resetTokenConfirm');
  const hosted = expandHostedLink(render());
  const confirm = confirmIn(hosted);
  const first = confirm.props.onConfirm();
  confirm.props.onConfirm(); // 双击的第二次被挡下
  await tick();
  assert.equal(resets.length, 1);
  assert.equal(confirmIn(expandHostedLink(render())).props.okButtonProps.loading, true);
  gate();
  await first;
  await tick();
  assert.deepEqual(messages.log.filter(([c]) => c === 'info'), [['info', 'regenerate.pending']]);
  assert.equal(confirmIn(expandHostedLink(render())).props.okButtonProps.loading, false);
  assert.equal(resets.length, 1);
});

test('regenerate outcomes clear stale generation errors and warnings', async () => {
  const state = hooks();
  const messages = messageLog();
  let step = 0;
  const api = async (url, options = {}) => {
    if (url.endsWith('/generate')) {
      step += 1;
      if (step === 1) throw new FakeApiError(400, { details: ['groups: unknown group "x"'] });
      return { ruleset_conflicts: ['c1'] };
    }
    return detail();
  };
  const module = detailModule(state, api, messages);
  const render = mountDetail(state, module);
  await tick();
  const tabs = (tree) => find(tree, (e) => e.type === 'Tabs').props.items;
  const bannerErr = (tree) => find(tree, (e) => e.props?.className === 'warn-banner error');
  const bannerWarn = (tree) => find(tree, (e) => e.props?.className === 'warn-banner');

  // 生成失败:非规则类错误以 banner 展示。
  let tree = render();
  await tabs(tree)[0].children.props.onRefresh();
  tree = render();
  assert.ok(bannerErr(tree));

  // applied:旧错误清除。
  tabs(tree)[3].children.props.onRegenerate({ status: 'applied' });
  tree = render();
  assert.equal(bannerErr(tree), null);

  // 再次生成成功并带回冲突警告。
  await tabs(tree)[0].children.props.onRefresh();
  tree = render();
  assert.ok(bannerWarn(tree));

  // 任意配置变更(pending):旧警告清除,且不产生错误。
  tabs(tree)[3].children.props.onRegenerate({ status: 'pending' });
  tree = render();
  assert.equal(bannerWarn(tree), null);
  assert.equal(bannerErr(tree), null);
});
