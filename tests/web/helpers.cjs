const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { createRequire } = require('node:module');
const root = path.resolve(__dirname, '../..');
const webRequire = createRequire(path.join(root, 'web/package.json'));
const ts = webRequire('typescript');
const React = webRequire('react');

// Execute the real TS/TSX module with controlled network/router/hook inputs.
// JSX remains real React elements; rendering assertions use react-dom/server.
function load(relativePath, mocks = {}, globals = {}) {
  const filename = path.join(root, 'web/src', relativePath);
  const source = ts.transpileModule(fs.readFileSync(filename, 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
  }).outputText;
  const module = { exports: {} };
  const req = (id) => Object.hasOwn(mocks, id) ? mocks[id]
    : id.endsWith('.css') ? {} : webRequire(id);
  vm.runInNewContext(source, { module, exports: module.exports, require: req, ...globals }, { filename });
  return module.exports;
}

// A narrow hook harness: retains state across rerenders and runs effect cleanup.
// This avoids adding a browser emulator just to test routing/request races.
function hooks(stateSeeds = []) {
  const slots = [];
  let cursor = 0;
  let stateCursor = 0;
  let pending = [];
  const sameDeps = (left, right) => left && right && left.length === right.length
    && left.every((dep, n) => Object.is(dep, right[n]));
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
    useEffect(effect, deps) {
      const i = cursor++;
      const old = slots[i];
      if (!old || !sameDeps(deps, old.deps)) {
        pending.push(() => {
          old?.cleanup?.();
          slots[i] = { deps, cleanup: effect() };
        });
      }
    },
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
    render(fn, props) { cursor = 0; stateCursor = 0; return fn(props); },
    effects() { const effects = pending; pending = []; effects.forEach((fn) => fn()); },
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

function deferred() {
  let resolve, reject;
  const promise = new Promise((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

const tick = () => new Promise(setImmediate);
const translation = { useTranslation: () => ({ t: (key) => key }) };
const icons = new Proxy({}, { get: () => () => null });
module.exports = { load, hooks, find, deferred, tick, translation, icons, React, webRequire };
