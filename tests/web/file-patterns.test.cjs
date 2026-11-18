const assert = require('node:assert/strict');
const { createRequire } = require('node:module');
const { test } = require('node:test');
const { webRequire } = require('./helpers.cjs');

test('Vite and its file walkers resolve the same path matcher', () => {
  const vite = createRequire(webRequire.resolve('vite'));
  const tinyglobby = createRequire(vite.resolve('tinyglobby'));
  const fdir = createRequire(tinyglobby.resolve('fdir'));
  for (const consumer of [vite, tinyglobby, fdir]) {
    assert.equal(consumer.resolve('picomatch'), webRequire.resolve('picomatch'));
  }
});

test('repeated filename wildcards compile once and preserve extension and directory boundaries', () => {
  const match = webRequire('picomatch');
  const optional = 'icons/?(icon-*).{svg,png}';
  const repeated = 'icons/*(icon-*).{svg,png}';
  assert.equal(match.makeRe(repeated).source, match.makeRe(optional).source);
  assert.equal(match(repeated)('icons/.svg'), true);

  const required = match('icons/+(icon-*).{svg,png}');
  assert.equal(required('icons/icon-home.svg'), true);
  assert.equal(required('icons/icon-backicon-next.png'), true);
  assert.equal(required('icons/.png'), false);
  assert.equal(required('icons/icon-home.jpg'), false);
  assert.equal(required('icons/icon-home/thumb.svg'), false);
});
