const assert = require('node:assert/strict');
const { test } = require('node:test');
const { load, hooks, find, tick, translation, icons, dndMocks } = require('./helpers.cjs');

// 表单层行为:节点/分组编辑器的键值累积、YAML 草稿、数值约束与类型输入。

const antdStub = {
  AutoComplete: 'AutoComplete',
  Button: 'Button',
  Divider: 'Divider',
  Form: Object.assign(function Form() {}, { Item: function FormItem() {} }),
  Input: Object.assign(function Input() {}, {
    TextArea: function TextArea() {},
    Password: function Password() {},
  }),
  InputNumber: 'InputNumber',
  Radio: { Group: 'RadioGroup' },
  Select: 'Select',
  Space: 'Space',
  Switch: 'Switch',
  Typography: { Paragraph: 'Paragraph', Text: 'Text' },
};

// 真实 fields.tsx(纯函数)+ 组件占位;NodeForm 依赖的纯函数保持真实实现。
function fieldsModule(react) {
  return load('components/fields.tsx', {
    ...(react ? { react } : {}),
    antd: antdStub,
    '@ant-design/icons': icons,
    'react-i18next': translation,
  });
}

function nodeFormModule() {
  const fields = fieldsModule(null);
  return load('components/NodeForm.tsx', {
    antd: antdStub,
    'react-i18next': translation,
    './nodeSchema': load('components/nodeSchema.ts'),
    './fields': {
      AdvancedFields: 'AdvancedFields',
      FieldInput: 'FieldInput',
      getPath: fields.getPath,
      isEmptyValue: fields.isEmptyValue,
      setPath: fields.setPath,
      advancedEntries: fields.advancedEntries,
    },
  });
}

test('node content keeps `__proto__` entries through a parse/serialize round trip', () => {
  const { contentToModel, modelToContent } = nodeFormModule();
  const model = contentToModel('name: n\ntype: ss\n__proto__:\n  a: 1\nserver: s\n');
  assert.equal(Object.prototype.hasOwnProperty.call(model.fields, '__proto__'), true);
  const again = contentToModel(modelToContent(model));
  assert.equal(Object.prototype.hasOwnProperty.call(again.fields, '__proto__'), true);
  assert.deepEqual(again.fields['__proto__'], { a: 1 });
  assert.equal({}.a, undefined); // 普通对象不受影响
});

test('advanced rows merge user keys like `__proto__` into node fields intact', () => {
  const { default: NodeForm, modelToContent, contentToModel } = nodeFormModule();
  let next = null;
  const tree = NodeForm({
    value: { name: 'n', type: 'ss', fields: { server: 's' } },
    onChange: (m) => { next = m; },
  });
  find(tree, (e) => e.type === 'AdvancedFields').props.onChange([['__proto__', { a: 1 }]]);
  assert.equal(Object.prototype.hasOwnProperty.call(next.fields, '__proto__'), true);
  const again = contentToModel(modelToContent(next));
  assert.deepEqual(again.fields['__proto__'], { a: 1 });
  assert.equal({}.a, undefined);
});

test('node type editor allows free input and keeps suggestion filtering', () => {
  const { default: NodeForm } = nodeFormModule();
  let next = null;
  const tree = NodeForm({
    value: { name: 'n', type: 'ss', fields: {} },
    onChange: (m) => { next = m; },
  });
  const ac = find(tree, (e) => e.type === 'AutoComplete');
  assert.ok(ac);
  assert.equal(ac.props.value, 'ss');
  ac.props.onChange('my-own-proto'); // 自由输入不在候选表里的类型
  assert.equal(next.type, 'my-own-proto');
  assert.equal(ac.props.filterOption('shadow', { label: 'Shadowsocks', value: 'ss' }), true);
  assert.equal(ac.props.filterOption('ss', { label: 'Shadowsocks', value: 'ss' }), true);
  assert.equal(ac.props.filterOption('xyz', { label: 'Shadowsocks', value: 'ss' }), false);
});

test('schema-driven numeric fields carry bounds and normalize input', () => {
  const nodeSchema = load('components/nodeSchema.ts');
  const groupSchema = load('pages/detail/groupSchema.ts');
  const fields = fieldsModule(null);
  const port = nodeSchema.BASE_FIELDS.find((f) => f.key === 'port');
  assert.deepEqual([port.min, port.max, port.precision], [1, 65535, 0]);
  const alterId = nodeSchema.commonFields('vmess').find((f) => f.key === 'alterId');
  assert.deepEqual([alterId.min, alterId.max, alterId.precision], [0, undefined, 0]);
  const urlTest = groupSchema.GROUP_OPTION_FIELDS['url-test'];
  const interval = urlTest.find((f) => f.key === 'interval');
  assert.deepEqual([interval.min, interval.precision], [1, 0]);
  const tolerance = urlTest.find((f) => f.key === 'tolerance');
  assert.deepEqual([tolerance.min, tolerance.precision], [0, 0]);
  // FieldInput 把约束传给数字输入框,并在写回模型前归一化输入值。
  const values = [];
  const el = fields.FieldInput({ def: port, value: 8080, onChange: (value) => values.push(value) });
  assert.equal(el.type, 'InputNumber');
  assert.deepEqual([el.props.min, el.props.max, el.props.precision], [1, 65535, 0]);
  for (const input of ['-5', '70000', '443.7', '']) el.props.onInput(input);
  el.props.onInput('not-a-number');
  assert.deepEqual(values, [1, 65535, 444, null]);
});

test('advanced number rows accept any numeric value without bounds', () => {
  const state = hooks();
  const fields = fieldsModule(state.react);
  const tree = state.render(fields.AdvancedFields, {
    entries: [['weight', 5]],
    reserved: new Set(),
    onChange: () => {},
  });
  const input = valueEditor(tree);
  assert.equal(input.type, 'InputNumber');
  assert.deepEqual([input.props.min, input.props.max, input.props.precision],
    [undefined, undefined, undefined]);
});

// AdvancedValue/ObjectField 是模块内部组件,harness 不执行;手动展开到最终的 antd 占位元素。
function valueEditor(tree) {
  const av = find(tree, (e) => e.props && 'kind' in e.props && 'onDraft' in e.props);
  let el = av.type(av.props);
  if (el && el.props && !('kind' in el.props) && 'onDraft' in el.props) el = el.type(el.props);
  return el;
}

// 模块在 vm 里执行,产出的数组/对象与测试字面量原型不同;比较前先 JSON 归一化。
const norm = (v) => JSON.parse(JSON.stringify(v));

test('yaml rows keep an unparseable draft locally instead of writing it back', () => {
  const state = hooks();
  const fields = fieldsModule(state.react);
  let current = [['extra', { a: 1 }]];
  const writes = [];
  const props = () => ({
    entries: current,
    reserved: new Set(),
    onChange: (rows) => { writes.push(rows); current = rows; },
  });
  let tree = state.render(fields.AdvancedFields, props());
  const kindSelect = () => find(tree, (e) => e.type === 'Select');
  assert.equal(valueEditor(tree).props.value, 'a: 1');

  // 输入非法 YAML:父级仍收到上一次有效值,文本框显示草稿原文。
  valueEditor(tree).props.onChange({ target: { value: '{broken' } });
  tree = state.render(fields.AdvancedFields, props());
  assert.equal(writes.length, 1);
  assert.deepEqual(norm(writes[0]), [['extra', { a: 1 }]]);
  assert.equal(valueEditor(tree).props.value, '{broken');

  // 继续输入直到解析成功,值才写回父级。
  valueEditor(tree).props.onChange({ target: { value: 'a: 2' } });
  tree = state.render(fields.AdvancedFields, props());
  assert.deepEqual(norm(writes[writes.length - 1]), [['extra', { a: 2 }]]);
  assert.equal(valueEditor(tree).props.value, 'a: 2');
});

test('kind switching converts from the current draft, not a stale parsed value', () => {
  const state = hooks();
  const fields = fieldsModule(state.react);
  let current = [['extra', { a: 1 }]];
  const writes = [];
  const props = () => ({
    entries: current,
    reserved: new Set(),
    onChange: (rows) => { writes.push(rows); current = rows; },
  });
  let tree = state.render(fields.AdvancedFields, props());
  const kindSelect = () => find(tree, (e) => e.type === 'Select');

  // 打出一段尚未解析的草稿,直接切到文本:文本值来自草稿而不是旧值。
  valueEditor(tree).props.onChange({ target: { value: '{broken' } });
  tree = state.render(fields.AdvancedFields, props());
  kindSelect().props.onChange('text');
  tree = state.render(fields.AdvancedFields, props());
  assert.deepEqual(norm(writes[writes.length - 1]), [['extra', '{broken']]);
  assert.equal(valueEditor(tree).props.value, '{broken');

  // 文本再切回 YAML:解析失败时只留草稿,值不被非法内容替换。
  kindSelect().props.onChange('yaml');
  tree = state.render(fields.AdvancedFields, props());
  assert.deepEqual(norm(writes[writes.length - 1]), [['extra', '{broken']]);
  assert.equal(valueEditor(tree).props.value, '{broken');
});

test('group form keeps `__proto__` advanced options through save and confirms once', async () => {
  const state = hooks();
  const posts = [];
  const sensors = [];
  let gate;
  const api = async (url, options = {}) => {
    if (url.endsWith('/proxies')) return { proxies: [], groups: [] };
    if ((options.method ?? 'GET') === 'POST') {
      posts.push(JSON.parse(options.body));
      return new Promise((res) => { gate = () => res({}); });
    }
    return [];
  };
  const { default: GroupsCard } = load('pages/detail/GroupsCard.tsx', {
    react: state.react,
    antd: {
      App: { useApp: () => ({ message: { success() {}, error() {}, info() {} } }) },
      Button: 'Button',
      Form: antdStub.Form,
      Input: 'Input',
      Modal: 'Modal',
      Popconfirm: 'Popconfirm',
      Select: 'Select',
    },
    '@ant-design/icons': icons,
    ...dndMocks(sensors),
    'react-i18next': translation,
    '../../api': { api, errorMessage: () => 'failed' },
    '../../components/fields': {
      AdvancedFields: 'AdvancedFields',
      FieldInput: 'FieldInput',
      TypeChips: 'TypeChips',
      advancedEntries: (obj, known) => Object.entries(obj).filter(([k]) => !known.has(k)),
    },
    '../../components/regenerate': { useRegenerateNotice: () => () => {} },
    '../../components/useSerialSave': {
      useSerialSave: () => async (run) => {
        try { return { ok: true, value: await run(), latest: true }; }
        catch (error) { return { ok: false, error, latest: true }; }
      },
    },
    './groupSchema': load('pages/detail/groupSchema.ts'),
  });
  const props = {
    profileId: 'p', groups: [], nodes: [], generatedAt: null,
    onSaved: () => {}, onRegenerate: () => {},
  };
  let tree = state.render(GroupsCard, props);
  state.effects();
  await tick();
  // 键盘与指针两种排序传感器都已注册
  assert.ok(sensors.includes('KeyboardSensor'));
  assert.ok(sensors.includes('PointerSensor'));

  find(tree, (e) => e.type === 'Button' && e.props?.children === 'groups.add').props.onClick();
  tree = state.render(GroupsCard, props);
  find(tree, (e) => e.type === 'Input').props.onChange({ target: { value: 'g1' } });
  tree = state.render(GroupsCard, props);
  find(tree, (e) => e.type === 'AdvancedFields').props.onChange([['__proto__', { a: 1 }]]);
  tree = state.render(GroupsCard, props);

  const modal = () => find(tree, (e) => e.type === 'Modal');
  const first = modal().props.onOk();
  modal().props.onOk(); // 双击的第二次被挡下
  assert.equal(posts.length, 1);
  tree = state.render(GroupsCard, props);
  assert.equal(modal().props.confirmLoading, true);
  gate();
  await first;
  tree = state.render(GroupsCard, props);
  assert.equal(modal().props.confirmLoading, false);
  assert.equal(posts.length, 1);
  assert.equal(posts[0].name, 'g1');
  assert.equal(Object.prototype.hasOwnProperty.call(posts[0].options, '__proto__'), true);
  assert.deepEqual(posts[0].options['__proto__'], { a: 1 });
  assert.equal({}.a, undefined);
});
