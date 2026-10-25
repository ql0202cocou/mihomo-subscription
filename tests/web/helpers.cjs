const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { createRequire } = require('node:module');
const root = path.resolve(__dirname, '../..');
const webRequire = createRequire(path.join(root, 'web/package.json'));
const ts = webRequire('typescript');
const React = webRequire('react');

const SOURCE_EXTENSIONS = ['', '.ts', '.tsx'];

// Resolve a relative import of web/src to its source file, or null when none exists.
function resolveSource(fromDir, id) {
  const base = path.resolve(fromDir, id);
  return SOURCE_EXTENSIONS.map((ext) => base + ext)
    .find((file) => fs.existsSync(file) && fs.statSync(file).isFile()) ?? null;
}

// Execute the real TS/TSX module with controlled network/router/hook inputs.
// JSX remains real React elements; rendering assertions use react-dom/server.
// Relative imports that are not mocked load the real web/src module with the same mocks;
// relative mock keys are matched by resolved path, so a nested module sees the mocks too.
function load(relativePath, mocks = {}, globals = {}) {
  const filename = path.join(root, 'web/src', relativePath);
  const relativeMocks = new Map();
  for (const [id, mock] of Object.entries(mocks)) {
    if (id.startsWith('.')) relativeMocks.set(path.resolve(path.dirname(filename), id), mock);
  }
  return loadFile(filename, mocks, relativeMocks, globals, new Map());
}

function loadFile(filename, mocks, relativeMocks, globals, cache) {
  if (cache.has(filename)) return cache.get(filename).exports;
  const source = ts.transpileModule(fs.readFileSync(filename, 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
    // 传真实文件名,TS 才按 .ts(而非 .tsx)解析,泛型箭头函数才不会被误当 JSX 编译。
    fileName: filename,
  }).outputText;
  const module = { exports: {} };
  cache.set(filename, module);
  const dir = path.dirname(filename);
  const req = (id) => {
    if (id.endsWith('.css')) return {};
    if (!id.startsWith('.')) return Object.hasOwn(mocks, id) ? mocks[id] : webRequire(id);
    const target = path.resolve(dir, id);
    if (relativeMocks.has(target)) return relativeMocks.get(target);
    const file = resolveSource(dir, id);
    if (!file) throw new Error(`cannot resolve ${id} from ${filename}`);
    return loadFile(file, mocks, relativeMocks, globals, cache);
  };
  vm.runInNewContext(source, { module, exports: module.exports, require: req, ...globals }, { filename });
  return module.exports;
}

// A narrow hook harness: retains state across rerenders and runs effect cleanup.
// This avoids adding a browser emulator just to test routing/request races.
function hooks(stateSeeds = []) {
  const slots = [];
  let cursor = 0;
  let stateCursor = 0;
  // 两个队列都要按名动态读取:冲刷时不可重新赋值数组,否则闭包里的旧引用会丢 effect。
  const queues = { passive: [], layout: [] };
  const sameDeps = (left, right) => left && right && left.length === right.length
    && left.every((dep, n) => Object.is(dep, right[n]));
  const effectSlot = (name) => (effect, deps) => {
    const i = cursor++;
    const old = slots[i];
    if (!old || !sameDeps(deps, old.deps)) {
      queues[name].push(() => {
        old?.cleanup?.();
        slots[i] = { deps, cleanup: effect() };
      });
    }
  };
  const react = {
    ...React,
    useState(initial) {
      const i = cursor++;
      const seed = stateCursor++;
      if (!slots[i]) slots[i] = { value: seed < stateSeeds.length ? stateSeeds[seed]
        : typeof initial === 'function' ? initial() : initial };
      return [slots[i].value, (next) => {
        slots[i].value = typeof next === 'function' ? next(slots[i].value) : next;
      }];
    },
    useRef(initial) {
      const i = cursor++;
      if (!slots[i]) slots[i] = { value: { current: initial } };
      return slots[i].value;
    },
    useEffect: effectSlot('passive'),
    // 布局副作用在「绘制」前同步执行:render 返回前冲刷,与真实 React 的先后一致。
    useLayoutEffect: effectSlot('layout'),
    useCallback(fn, deps) {
      const i = cursor++;
      if (!slots[i] || !sameDeps(deps, slots[i].deps)) slots[i] = { deps, value: fn };
      return slots[i].value;
    },
    useMemo(fn, deps) {
      const i = cursor++;
      if (!slots[i] || !sameDeps(deps, slots[i].deps)) slots[i] = { deps, value: fn() };
      return slots[i].value;
    },
  };
  return {
    react,
    render(fn, props) {
      cursor = 0; stateCursor = 0;
      const out = fn(props);
      queues.layout.splice(0).forEach((fn) => fn());
      return out;
    },
    effects() { queues.passive.splice(0).forEach((fn) => fn()); },
    unmount() { slots.forEach((slot) => slot.cleanup?.()); },
  };
}

function find(element, predicate) {
  if (!element || typeof element !== 'object') return null;
  if (predicate(element)) return element;
  for (const child of React.Children.toArray(element.props?.children)) {
    const result = find(child, predicate);
    if (result) return result;
  }
  return null;
}

// Collect every element in the tree that matches `predicate`, depth-first.
function findAll(element, predicate, out = []) {
  if (!element || typeof element !== 'object') return out;
  if (predicate(element)) out.push(element);
  for (const child of React.Children.toArray(element.props?.children)) {
    findAll(child, predicate, out);
  }
  return out;
}

function deferred() {
  let resolve, reject;
  const promise = new Promise((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

const tick = () => new Promise(setImmediate);
const passthrough = ({ children }) => children;
const arrayMove = (arr, from, to) => {
  const next = arr.slice();
  const [moved] = next.splice(from, 1);
  next.splice(to, 0, moved);
  return next;
};
// '@dnd-kit/*' stand-ins whose sensors are recorded by name into `sensors`.
const dndMocks = (sensors) => ({
  '@dnd-kit/core': {
    DndContext: passthrough,
    PointerSensor: 'PointerSensor',
    KeyboardSensor: 'KeyboardSensor',
    closestCenter: () => {},
    useSensor: (s) => (sensors.push(s), {}),
    useSensors: () => [],
  },
  '@dnd-kit/sortable': {
    SortableContext: passthrough,
    arrayMove,
    sortableKeyboardCoordinates: () => {},
    useSortable: () => ({ attributes: {}, listeners: {}, setNodeRef: () => {} }),
    verticalListSortingStrategy: {},
  },
  '@dnd-kit/utilities': { CSS: { Transform: { toString: () => undefined } } },
});
const translation = { useTranslation: () => ({ t: (key) => key }) };
const icons = new Proxy({}, { get: () => () => null });
module.exports = {
  load, hooks, find, findAll, deferred, tick, translation, icons, React, webRequire,
  passthrough, arrayMove, dndMocks,
};
