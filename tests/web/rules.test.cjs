const assert = require('node:assert/strict');
const { test } = require('node:test');
const {
  load, hooks, find, findAll, deferred, tick, translation, icons, passthrough, arrayMove,
} = require('./helpers.cjs');

// 装配与 detail_state.test.cjs 同风格,但 useSerialSave / ruleRefs / groupSchema /
// ruleSetConstants 一律用真实模块,只替身 antd、图标与 dnd-kit。
// 列表行(SortableRuleRow)带 onRemove;钉底的 MATCH 行(MatchRow)只有 onEdit。
const isRow = (e) => typeof e.props?.line === 'string' && typeof e.props?.onRemove === 'function';
const isMatchRow = (e) =>
  typeof e.props?.line === 'string' &&
  typeof e.props?.onEdit === 'function' &&
  typeof e.props?.onRemove !== 'function';
const rowLines = (tree) => findAll(tree, isRow).map((e) => e.props.line);
const editModal = (tree) =>
  find(tree, (e) => e.type === 'Modal' && (e.props.title === 'rules.add' || e.props.title === 'rules.edit'));
const importModal = (tree) => find(tree, (e) => e.type === 'Modal' && e.props.title === 'rules.importHosted');
const rulePuts = (requests) =>
  requests.filter((r) => r.method === 'PUT' && r.url.endsWith('/rules')).map((r) => r.body.content);
const definitionWrites = (requests) =>
  requests.filter(
    (r) => (r.method === 'PUT' || r.method === 'POST') && r.url.includes('/rule-sets') && !r.url.endsWith('/import'),
  );

function profileRuleSet(overrides = {}) {
  return {
    id: 'rs1', name: 'ads', behavior: 'domain', format: 'yaml', source: 'manual',
    content: '', enabled: true, count: 0, url: '', remote_url_masked: null,
    interval_hours: 24, cache: true, last_fetch_status: null,
    created_at: '', updated_at: '', ...overrides,
  };
}

function globalRuleSet(overrides = {}) {
  return {
    id: 'g1', name: 'ads', behavior: 'domain', format: 'yaml', source: 'manual',
    content: '', enabled: true, count: 1, remote_url_masked: null, interval_hours: 24,
    cache: false, created_at: '', updated_at: '', ...overrides,
  };
}

function session(initial, options = {}) {
  const state = hooks();
  const calls = { info: [], success: [], error: [], warning: [] };
  const message = {
    info: (text) => calls.info.push(text),
    success: (text) => calls.success.push(text),
    error: (text) => calls.error.push(text),
    warning: (text) => calls.warning.push(text),
  };
  const requests = [];
  const input = Object.assign(() => null, { TextArea: () => null });
  const handlers = options.handlers ?? {};
  const api = async (url, opts = {}) => {
    const method = opts.method ?? 'GET';
    const request = { method, url, body: opts.body ? JSON.parse(opts.body) : undefined };
    requests.push(request);
    const handler = handlers[`${method} ${url}`];
    if (handler) return handler(request);
    if (url.endsWith('/proxies')) return { proxies: [], groups: [] };
    if (url === '/api/rule-sets') return options.ruleSets ?? [];
    if (method === 'GET' && url.endsWith('/rule-sets')) return options.profileRuleSets ?? [];
    if (url.endsWith('/provider-rules')) return { rules: [] };
    if (method === 'PUT' && url.endsWith('/rules')) return { regenerate: { status: 'applied' } };
    if ((method === 'PUT' || method === 'POST') && url.includes('/rule-sets')) return {};
    throw new Error(`unexpected request: ${method} ${url}`);
  };
  const dnd = options.dnd ?? {};
  const { default: RulesCard } = load('pages/detail/RulesCard.tsx', {
    react: state.react,
    antd: {
      App: { useApp: () => ({ message }) },
      AutoComplete: 'AutoComplete', Button: 'Button', Checkbox: 'Checkbox',
      Input: input, InputNumber: 'InputNumber', Modal: 'Modal', Popconfirm: 'Popconfirm',
      Segmented: 'Segmented', Select: 'Select', Switch: 'Switch',
      Typography: { Text: 'Text' },
    },
    '@ant-design/icons': icons,
    '@dnd-kit/core': {
      DndContext: passthrough,
      PointerSensor: dnd.PointerSensor ?? {},
      KeyboardSensor: dnd.KeyboardSensor ?? {},
      closestCenter: () => {},
      useSensor: dnd.useSensor ?? (() => {}),
      useSensors: dnd.useSensors ?? (() => []),
    },
    '@dnd-kit/sortable': {
      SortableContext: passthrough, arrayMove,
      sortableKeyboardCoordinates: dnd.sortableKeyboardCoordinates ?? (() => {}),
      verticalListSortingStrategy: {},
      useSortable: () => ({ attributes: {}, listeners: {}, setNodeRef: () => {} }),
    },
    '@dnd-kit/utilities': { CSS: { Transform: { toString: () => undefined } } },
    'react-i18next': translation,
    '../../api': { api, errorMessage: () => 'failed' },
    '../../components/regenerate': { useRegenerateNotice: () => () => {} },
    '../../components/useSerialSave': load('components/useSerialSave.ts', { react: state.react }),
    '../../components/ruleSetConstants': load('components/ruleSetConstants.ts'),
    '../../components/fields': { TypeChips: 'TypeChips' },
    './groupSchema': load('pages/detail/groupSchema.ts'),
    './ruleRefs': load('pages/detail/ruleRefs.ts'),
  });
  const props = {
    profileId: 'p', initial, nodes: [], groups: [], generatedAt: null, errors: [],
    onSaved: options.onSaved ?? (() => {}), onRegenerate: () => {},
  };
  const render = () => {
    const tree = state.render(RulesCard, props);
    state.effects();
    return tree;
  };
  return {
    calls, requests, input,
    async mount() {
      render();
      await tick();
      render();
      await tick();
    },
    render,
    setInitial(value) {
      props.initial = value;
      return render();
    },
    row(tree, line) {
      const element = find(tree, (e) => isRow(e) && e.props.line === line);
      assert.ok(element, `rule row not found: ${line}`);
      return element;
    },
    // 钻取当前编辑弹窗内的 RuleComposer / RuleSetDefBlock(都是 RulesCard 内部组件,
    // 需用其元素再渲染一层才能拿到字段的 onChange)。
    drill() {
      const tree = render();
      const modal = editModal(tree);
      assert.ok(modal, 'edit modal not found');
      const composer = find(modal, (e) => typeof e.type === 'function' && e.props?.policyOptions && e.props?.model);
      assert.ok(composer, 'rule composer not found');
      const composerTree = state.render(composer.type, composer.props);
      const defBlock = find(
        composerTree,
        (e) => typeof e.type === 'function' && e.props?.model && e.props?.ruleSets && !e.props?.policyOptions,
      );
      const defTree = defBlock ? state.render(defBlock.type, defBlock.props) : null;
      return { tree, modal, composer, composerTree, defBlock, defTree };
    },
    nameInput(defTree) {
      const element = find(defTree, (e) => e.props?.placeholder === 'rules.ruleSetPayloadHint');
      assert.ok(element, 'rule-set name input not found');
      return element;
    },
    contentArea(defTree) {
      const element = find(defTree, (e) => e.type === input.TextArea);
      assert.ok(element, 'rule-set content textarea not found');
      return element;
    },
  };
}

// 打开「导入托管规则」弹窗并勾选指定规则集,返回确认按钮触发的导入 Promise。
function startHostedImport(s, name) {
  let tree = s.render();
  find(tree, (e) => e.type === 'Button' && e.props.children === 'rules.importHosted').props.onClick();
  tree = s.render();
  const checkbox = find(tree, (e) => e.type === 'Checkbox' && e.props['aria-label'] === name);
  assert.ok(checkbox, `checkbox not found: ${name}`);
  checkbox.props.onChange({ target: { checked: true } });
  tree = s.render();
  return importModal(tree).props.onOk();
}

test('saving an untouched RULE-SET reference sends no definition request', async () => {
  // 机场 provider 自带的规则集:本地 ③ 库不存在,仅保存引用行。
  const plain = session('RULE-SET,plane,DIRECT\nMATCH,DIRECT');
  await plain.mount();
  plain.row(plain.render(), 'RULE-SET,plane,DIRECT').props.onEdit();
  await plain.drill().modal.props.onOk();
  await tick();
  assert.deepEqual(rulePuts(plain.requests), ['RULE-SET,plane,DIRECT\nMATCH,DIRECT']);
  assert.deepEqual(definitionWrites(plain.requests), []);

  // 本地 ③ 库存在同名定义、编辑弹窗预填了定义但未改动:同样不写回定义。
  const managed = session('RULE-SET,ads,DIRECT\nMATCH,DIRECT', {
    profileRuleSets: [profileRuleSet({ content: '- ads.example' })],
  });
  await managed.mount();
  managed.row(managed.render(), 'RULE-SET,ads,DIRECT').props.onEdit();
  const drilled = managed.drill();
  assert.equal(drilled.composer.props.model.rsManage, true); // 预填定义并开启管理
  assert.equal(drilled.composer.props.model.rsDirty, false);
  await drilled.modal.props.onOk();
  await tick();
  assert.deepEqual(rulePuts(managed.requests), ['RULE-SET,ads,DIRECT\nMATCH,DIRECT']);
  assert.deepEqual(definitionWrites(managed.requests), []);
});

test('renaming a RULE-SET reference to an existing local name changes only the rule line', async () => {
  const s = session('RULE-SET,ads2,DIRECT\nMATCH,DIRECT', {
    profileRuleSets: [
      profileRuleSet({ id: 'rs1', name: 'ads', content: '- ads.example' }),
      profileRuleSet({ id: 'rs2', name: 'ads2', content: '- old.example' }),
    ],
  });
  await s.mount();
  s.row(s.render(), 'RULE-SET,ads2,DIRECT').props.onEdit();
  let d = s.drill();
  assert.equal(d.composer.props.model.rsLoadedName, 'ads2');
  s.nameInput(d.defTree).props.onChange({ target: { value: 'ads' } });
  d = s.drill();
  // 改名命中已有定义:编辑内容切换为 ads 的定义,且不算用户改过。
  assert.equal(d.composer.props.model.rsContent, '- ads.example');
  assert.equal(d.composer.props.model.rsLoadedName, 'ads');
  assert.equal(d.composer.props.model.rsDirty, false);
  await d.modal.props.onOk();
  await tick();
  // 规则行正常变更为 ads,但不 PUT 覆盖 ads 的已有定义。
  assert.deepEqual(rulePuts(s.requests), ['RULE-SET,ads,DIRECT\nMATCH,DIRECT']);
  assert.deepEqual(definitionWrites(s.requests), []);
});

test('definition writes happen only after explicit edits, before the rules save', async () => {
  const s = session('RULE-SET,ads,DIRECT\nMATCH,DIRECT', {
    profileRuleSets: [profileRuleSet({ id: 'rs1', name: 'ads', content: '- ads.example', enabled: false })],
  });
  await s.mount();
  s.row(s.render(), 'RULE-SET,ads,DIRECT').props.onEdit();
  let d = s.drill();
  s.contentArea(d.defTree).props.onChange({ target: { value: '- new.example' } });
  d = s.drill();
  assert.equal(d.composer.props.model.rsDirty, true);
  await d.modal.props.onOk();
  await tick();
  const writes = definitionWrites(s.requests);
  assert.deepEqual(writes.map((r) => [r.method, r.url]), [['PUT', '/api/profiles/p/rule-sets/rs1']]);
  assert.equal(writes[0].body.content, '- new.example');
  assert.equal(writes[0].body.enabled, false); // 更新沿用已有 enabled
  // 定义写回 → 重新拉取 ③ 库 → 保存规则行,顺序固定。
  const order = s.requests.map((r) => `${r.method} ${r.url}`);
  const at = (x) => order.indexOf(x);
  assert.ok(at('PUT /api/profiles/p/rule-sets/rs1') < at('PUT /api/profiles/p/rules'));
  assert.ok(
    order.lastIndexOf('GET /api/profiles/p/rule-sets') > at('PUT /api/profiles/p/rule-sets/rs1') &&
      order.lastIndexOf('GET /api/profiles/p/rule-sets') < at('PUT /api/profiles/p/rules'),
  );
  assert.deepEqual(rulePuts(s.requests), ['RULE-SET,ads,DIRECT\nMATCH,DIRECT']);
});

test('adding a managed RULE-SET creates the definition, then saves the rule line', async () => {
  const s = session('DOMAIN,a.example,DIRECT\nMATCH,DIRECT');
  await s.mount();
  let tree = s.render();
  find(tree, (e) => e.type === 'Button' && e.props.children === 'rules.add').props.onClick();
  let d = s.drill();
  d.composer.props.onChange({ ...d.composer.props.model, type: 'RULE-SET' });
  d = s.drill();
  s.nameInput(d.defTree).props.onChange({ target: { value: 'myset' } });
  d = s.drill();
  find(d.defTree, (e) => e.type === 'Switch').props.onChange(true); // 开启管理本地定义
  d = s.drill();
  s.contentArea(d.defTree).props.onChange({ target: { value: '- one.example' } });
  d = s.drill();
  d.composer.props.onChange({ ...d.composer.props.model, policy: 'DIRECT' });
  d = s.drill();
  await d.modal.props.onOk();
  await tick();
  const writes = definitionWrites(s.requests);
  assert.deepEqual(writes.map((r) => [r.method, r.url]), [['POST', '/api/profiles/p/rule-sets']]);
  assert.equal(writes[0].body.name, 'myset');
  assert.equal(writes[0].body.enabled, true); // 新建默认启用
  assert.equal(writes[0].body.content, '- one.example');
  assert.deepEqual(rulePuts(s.requests), ['DOMAIN,a.example,DIRECT\nRULE-SET,myset,DIRECT\nMATCH,DIRECT']);
});

test('changing a plain rule to MATCH removes the original line and moves the pin', async () => {
  const s = session('DOMAIN,a.example,DIRECT\nMATCH,REJECT');
  await s.mount();
  s.row(s.render(), 'DOMAIN,a.example,DIRECT').props.onEdit();
  let d = s.drill();
  d.composer.props.onChange({ ...d.composer.props.model, type: 'MATCH', policy: 'DIRECT' });
  d = s.drill();
  await d.modal.props.onOk();
  await tick();
  assert.deepEqual(rulePuts(s.requests), ['MATCH,DIRECT']);
  const tree = s.render();
  assert.deepEqual(rowLines(tree), []);
  assert.equal(find(tree, isMatchRow).props.line, 'MATCH,DIRECT');
});

test('changing the pinned MATCH to a plain rule appends it and clears the pin', async () => {
  const s = session('DOMAIN,a.example,DIRECT\nMATCH,REJECT');
  await s.mount();
  const tree = s.render();
  const pinned = find(tree, (e) => isMatchRow(e) && e.props.line === 'MATCH,REJECT');
  assert.ok(pinned, 'pinned MATCH row not found');
  pinned.props.onEdit();
  let d = s.drill();
  assert.equal(d.composer.props.model.type, 'MATCH');
  d.composer.props.onChange({ ...d.composer.props.model, type: 'DOMAIN', payload: 'b.example', policy: 'DIRECT' });
  d = s.drill();
  await d.modal.props.onOk();
  await tick();
  assert.deepEqual(rulePuts(s.requests), ['DOMAIN,a.example,DIRECT\nDOMAIN,b.example,DIRECT']);
  assert.equal(find(s.render(), isMatchRow), null);
});

test('with several MATCH lines only the last is pinned; editing an earlier one keeps the pin', async () => {
  const s = session('MATCH,DIRECT\nDOMAIN,a.example,DIRECT\nMATCH,REJECT');
  await s.mount();
  let tree = s.render();
  // 仅最后一条 MATCH 钉底,较早的 MATCH 作为普通行保留。
  assert.deepEqual(rowLines(tree), ['MATCH,DIRECT', 'DOMAIN,a.example,DIRECT']);
  assert.equal(find(tree, isMatchRow).props.line, 'MATCH,REJECT');
  assert.deepEqual(rulePuts(s.requests), []); // 加载本身不触发保存
  // 编辑较早的 MATCH 行:只改该行,钉底行不丢。
  s.row(tree, 'MATCH,DIRECT').props.onEdit();
  let d = s.drill();
  d.composer.props.onChange({ ...d.composer.props.model, policy: 'REJECT-DROP' });
  d = s.drill();
  await d.modal.props.onOk();
  await tick();
  assert.deepEqual(rulePuts(s.requests), ['MATCH,REJECT-DROP\nDOMAIN,a.example,DIRECT\nMATCH,REJECT']);
  tree = s.render();
  assert.deepEqual(rowLines(tree), ['MATCH,REJECT-DROP', 'DOMAIN,a.example,DIRECT']);
  assert.equal(find(tree, isMatchRow).props.line, 'MATCH,REJECT');
});

test('newline in type, payload or policy is rejected without any save or definition request', async () => {
  const s = session('DOMAIN,a.example,DIRECT\nMATCH,DIRECT');
  await s.mount();
  const tree = s.render();
  find(tree, (e) => e.type === 'Button' && e.props.children === 'rules.add').props.onClick();
  let d = s.drill();
  const base = { ...d.composer.props.model, payload: 'b.example', policy: 'DIRECT' };
  for (const broken of [
    { ...base, type: 'DOMAIN\nSUFFIX' },
    { ...base, payload: 'b.example\nc.example' },
    { ...base, policy: 'DIRECT\nREJECT' },
  ]) {
    d.composer.props.onChange(broken);
    d = s.drill();
    await d.modal.props.onOk();
  }
  await tick();
  assert.deepEqual(s.calls.error, ['rules.singleLine', 'rules.singleLine', 'rules.singleLine']);
  assert.deepEqual(s.requests.filter((r) => r.method !== 'GET'), []);
  // 修正后可以正常保存。
  d.composer.props.onChange(base);
  d = s.drill();
  await d.modal.props.onOk();
  await tick();
  assert.deepEqual(rulePuts(s.requests), ['DOMAIN,a.example,DIRECT\nDOMAIN-SUFFIX,b.example,DIRECT\nMATCH,DIRECT']);
});

test('ruleSetRefs matches the converter for plain, nested, comment and malformed lines', () => {
  const { ruleSetRefs } = load('pages/detail/ruleRefs.ts');
  const cases = [
    // 顶层引用
    ['RULE-SET,ads,DIRECT', ['ads']],
    ['rule-set,ads,direct', ['ads']],
    [' RULE-SET, ads , DIRECT ', ['ads']],
    ['RULE-SET', []],
    ['RULE-SET,ads', []],
    // 注释与空行
    ['# RULE-SET,ads,DIRECT', []],
    ['   # comment', []],
    ['', []],
    // 非引用类型
    ['MATCH,DIRECT', []],
    ['DOMAIN,a.com,DIRECT', []],
    // AND/OR/NOT 嵌套
    ['AND,((RULE-SET,ads),(NETWORK,tcp)),DIRECT', ['ads']],
    ['OR,((RULE-SET,ads),(RULE-SET,cdn)),DIRECT', ['ads', 'cdn']],
    ['NOT,((RULE-SET,ads)),DIRECT', ['ads']],
    ['and,((rule-set,ads)),direct', ['ads']],
    ['AND,((AND,((RULE-SET,deep))),(RULE-SET,top)),DIRECT', ['deep', 'top']],
    // 逻辑内不支持 SUB-RULE,只有平级的 RULE-SET 被收集
    ['AND,((SUB-RULE,(RULE-SET,x,DIRECT)),(RULE-SET,y)),DIRECT', ['y']],
    // SUB-RULE 的行级 payload 再包一层括号解析
    ['SUB-RULE,(RULE-SET,sub1,DIRECT),s', ['sub1']],
    // 单层括号恰好覆盖整个 payload 时不下钻(与 Mihomo 一致)
    ['AND,(RULE-SET,not-collected),DIRECT', []],
    ['NOT,(RULE-SET,single),DIRECT', []],
    // 畸形输入按无引用处理
    ['AND,((RULE-SET,ads),DIRECT', []],
    ['AND,RULE-SET,ads,DIRECT', []],
    ['AND', []],
    ['AND,((RULE-SET),(RULE-SET,ok)),DIRECT', []],
    ['AND,((RULE-SET,a),(OR,RULE-SET,b)),DIRECT', []],
    ['AND,((RULE-SET,ads)),REJECT,no-resolve', []],
    ['SUB-RULE,(AND,((RULE-SET,nested),(DOMAIN,a.com)),DIRECT),s', []],
  ];
  let nested = '((RULE-SET,shallow),(DOMAIN,a.com))';
  for (let depth = 1; depth < 16; depth++) nested = `((AND,${nested}),(DOMAIN,b.com))`;
  cases.push([`AND,${nested},DIRECT`, ['shallow']]);
  nested = `((AND,${nested}),(DOMAIN,b.com))`;
  cases.push([`AND,${nested},DIRECT`, []]);
  for (const [line, expected] of cases) {
    // ruleSetRefs 在 vm 上下文里构造数组,拷回本域再比较。
    assert.deepEqual([...ruleSetRefs(line)], expected, `line: ${line}`);
  }
});

test('rule sets already referenced inside nested logic rules cannot be picked for import', async () => {
  const s = session('AND,((RULE-SET,nested),(DOMAIN,a.example)),DIRECT\nMATCH,DIRECT', {
    ruleSets: [globalRuleSet({ name: 'nested' }), globalRuleSet({ id: 'g2', name: 'free' })],
  });
  await s.mount();
  let tree = s.render();
  find(tree, (e) => e.type === 'Button' && e.props.children === 'rules.importHosted').props.onClick();
  tree = s.render();
  const nested = find(tree, (e) => e.type === 'Checkbox' && e.props['aria-label'] === 'nested');
  const free = find(tree, (e) => e.type === 'Checkbox' && e.props['aria-label'] === 'free');
  assert.equal(nested.props.disabled, true); // 嵌套引用也算已引用,禁选
  assert.equal(nested.props.checked, true);
  assert.equal(free.props.disabled, false);
  assert.equal(free.props.checked, false);
  // 即使强制勾选,导入仍按引用状态过滤,不发请求。
  nested.props.onChange({ target: { checked: true } });
  tree = s.render();
  await importModal(tree).props.onOk();
  await tick();
  assert.deepEqual(s.calls.info, ['rules.importHostedNone']);
  assert.equal(s.requests.some((r) => r.url.endsWith('/import')), false);
});

test('hosted import queues behind an in-flight rules save and adopts the response content', async () => {
  const save = deferred();
  const importRequest = deferred();
  let saves = 0;
  const s = session('DOMAIN,a.example,DIRECT\nDOMAIN,b.example,DIRECT\nMATCH,DIRECT', {
    ruleSets: [globalRuleSet({ name: 'ads' })],
    handlers: {
      'PUT /api/profiles/p/rules': () =>
        ++saves === 1 ? save.promise : { regenerate: { status: 'applied' } },
      'POST /api/profiles/p/rule-sets/import': () => importRequest.promise,
    },
  });
  await s.mount();
  // 先发起一次规则保存(PUT 挂起),再发起托管导入:导入与规则保存共用串行队列。
  let tree = s.render();
  s.row(tree, 'DOMAIN,a.example,DIRECT').props.onRemove();
  await tick();
  assert.deepEqual(rulePuts(s.requests), ['DOMAIN,b.example,DIRECT\nMATCH,DIRECT']);

  find(tree, (e) => e.type === 'Button' && e.props.children === 'rules.importHosted').props.onClick();
  tree = s.render();
  find(tree, (e) => e.type === 'Checkbox' && e.props['aria-label'] === 'ads')
    .props.onChange({ target: { checked: true } });
  tree = s.render();
  const importJob = importModal(tree).props.onOk();
  await tick();
  assert.equal(s.requests.some((r) => r.url.endsWith('/import')), false); // 导入排在保存之后,未抢跑

  // 导入排队/在途期间,其它规则编辑(删除、编辑、拖拽、导入机场规则)都被阻止。
  tree = s.render();
  s.row(tree, 'DOMAIN,b.example,DIRECT').props.onRemove();
  s.row(tree, 'DOMAIN,b.example,DIRECT').props.onEdit();
  find(tree, (e) => e.props?.onDragEnd).props.onDragEnd({ active: { id: 'x' }, over: { id: 'y' } });
  find(tree, (e) => e.props?.title === 'rules.importConfirm').props.onConfirm();
  await tick();
  assert.deepEqual(rulePuts(s.requests), ['DOMAIN,b.example,DIRECT\nMATCH,DIRECT']); // 没有新的保存
  assert.equal(editModal(s.render()).props.open, false); // 编辑弹窗没有打开
  assert.equal(s.requests.some((r) => r.url.endsWith('/provider-rules')), false);

  // 第一个保存完成后导入才发出。
  save.resolve({ regenerate: { status: 'applied' } });
  await tick();
  await tick();
  const sent = s.requests.find((r) => r.url.endsWith('/import'));
  assert.deepEqual(sent.body, { names: ['ads'], policy: 'DIRECT' });
  // 响应的 rules_content 成为本地与已保存状态;后续保存包含导入行。
  importRequest.resolve({
    imported: 1, regenerate: { status: 'applied' },
    rules_content: 'DOMAIN,b.example,DIRECT\nRULE-SET,ads,DIRECT\nMATCH,DIRECT',
  });
  await importJob;
  await tick();
  assert.deepEqual(s.calls.success, ['rules.importHostedDone']);
  tree = s.render();
  assert.deepEqual(rowLines(tree), ['DOMAIN,b.example,DIRECT', 'RULE-SET,ads,DIRECT']);
  s.row(tree, 'DOMAIN,b.example,DIRECT').props.onRemove();
  await tick();
  assert.deepEqual(rulePuts(s.requests), [
    'DOMAIN,b.example,DIRECT\nMATCH,DIRECT',
    'RULE-SET,ads,DIRECT\nMATCH,DIRECT',
  ]);
});

test('hosted import ignores stale parent reloads and waits for synchronization', async () => {
  const initial = 'DOMAIN,a.example,DIRECT\nMATCH,DIRECT';
  const latest = 'DOMAIN,a.example,DIRECT\nRULE-SET,ads,DIRECT\nMATCH,DIRECT';
  const importRequest = deferred();
  const parentReload = deferred();
  let reloads = 0;
  const s = session(initial, {
    ruleSets: [globalRuleSet({ name: 'ads' })],
    handlers: { 'POST /api/profiles/p/rule-sets/import': () => importRequest.promise },
    onSaved: () => {
      reloads++;
      return parentReload.promise;
    },
  });
  await s.mount();
  const job = startHostedImport(s, 'ads');
  await tick();
  importRequest.resolve({ imported: 1, regenerate: { status: 'applied' }, rules_content: latest });
  await tick();
  await tick();
  assert.equal(reloads, 1);

  // 较早的父页面刷新晚到:不得覆盖导入响应,同步完成前也不能发起新保存。
  let tree = s.setInitial(initial);
  assert.deepEqual(rowLines(tree), ['DOMAIN,a.example,DIRECT', 'RULE-SET,ads,DIRECT']);
  s.row(tree, 'DOMAIN,a.example,DIRECT').props.onRemove();
  await tick();
  assert.deepEqual(rulePuts(s.requests), []);

  parentReload.resolve();
  await job;
  tree = s.setInitial(latest);
  s.row(tree, 'DOMAIN,a.example,DIRECT').props.onRemove();
  await tick();
  assert.deepEqual(rulePuts(s.requests), ['RULE-SET,ads,DIRECT\nMATCH,DIRECT']);
});

test('hosted import with nothing imported reports info when content is unchanged', async () => {
  const posts = [];
  const s = session('DOMAIN,a.example,DIRECT\nMATCH,DIRECT', {
    ruleSets: [globalRuleSet({ name: 'ads' }), globalRuleSet({ id: 'g2', name: 'cdn' })],
    handlers: {
      'POST /api/profiles/p/rule-sets/import': (r) => {
        posts.push(r.body);
        return posts.length === 1
          ? { imported: 0, regenerate: null, rules_content: 'DOMAIN,a.example,DIRECT\nMATCH,DIRECT' }
          : {
              imported: 0, regenerate: null,
              rules_content: 'DOMAIN,a.example,DIRECT\nRULE-SET,cdn,DIRECT\nMATCH,DIRECT',
            };
      },
    },
  });
  await s.mount();
  // imported=0 且内容无变化:提示 info,不是 success。
  await startHostedImport(s, 'ads');
  await tick();
  assert.deepEqual(s.calls.info, ['rules.importHostedNone']);
  assert.deepEqual(s.calls.success, []);
  // imported=0 但引用行有变化:按内容变化提示成功。
  await startHostedImport(s, 'cdn');
  await tick();
  assert.deepEqual(s.calls.success, ['rules.importHostedReferenced']);
  assert.deepEqual(s.calls.info, ['rules.importHostedNone']);
});

test('repeated submits while a definition write is in flight are ignored', async () => {
  const write = deferred();
  const s = session('RULE-SET,ads,DIRECT\nMATCH,DIRECT', {
    profileRuleSets: [profileRuleSet({ id: 'rs1', name: 'ads', content: '- ads.example' })],
    handlers: { 'PUT /api/profiles/p/rule-sets/rs1': () => write.promise },
  });
  await s.mount();
  s.row(s.render(), 'RULE-SET,ads,DIRECT').props.onEdit();
  let d = s.drill();
  s.contentArea(d.defTree).props.onChange({ target: { value: '- new.example' } });
  d = s.drill();
  const first = d.modal.props.onOk();
  await tick();
  // 定义写回在途期间再次提交:被忽略,不产生第二个定义请求。
  const again = s.drill().modal.props.onOk();
  await again;
  await tick();
  assert.equal(
    s.requests.filter((r) => r.method === 'PUT' && r.url.endsWith('/rule-sets/rs1')).length,
    1,
  );
  assert.deepEqual(rulePuts(s.requests), []); // 规则保存等定义写回完成后才发出
  write.resolve({});
  await first;
  await tick();
  assert.deepEqual(rulePuts(s.requests), ['RULE-SET,ads,DIRECT\nMATCH,DIRECT']);
});

test('drag sorting registers a pointer sensor and a keyboard sensor', async () => {
  const used = [];
  const PointerSensor = {};
  const KeyboardSensor = {};
  const coordinateGetter = () => {};
  const s = session('MATCH,DIRECT', {
    dnd: {
      PointerSensor, KeyboardSensor,
      useSensor: (sensor, options) => used.push({ sensor, options }),
      useSensors: (...list) => list,
      sortableKeyboardCoordinates: coordinateGetter,
    },
  });
  await s.mount();
  assert.ok(used.length >= 2);
  assert.equal(used[0].sensor, PointerSensor);
  // options 对象在 vm 上下文里构造,逐字段比较。
  assert.deepEqual(Object.keys(used[0].options), ['activationConstraint']);
  assert.equal(used[0].options.activationConstraint.distance, 4);
  assert.equal(used[1].sensor, KeyboardSensor);
  assert.deepEqual(Object.keys(used[1].options), ['coordinateGetter']);
  assert.equal(used[1].options.coordinateGetter, coordinateGetter);
});
