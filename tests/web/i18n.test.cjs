const assert = require('node:assert/strict');
const { test } = require('node:test');
const { load, React, webRequire } = require('./helpers.cjs');
const { renderToStaticMarkup } = webRequire('react-dom/server');
const { useTranslation } = webRequire('react-i18next');

const i18n = load('i18n.ts').default;

test('Chinese labels are ready at startup and remain the language fallback', () => {
  assert.equal(i18n.isInitialized, true);
  assert.equal(i18n.t('login.title'), '管理员登录');
  assert.equal(i18n.t('login.title', { lng: 'en' }), '管理员登录');
  assert.equal(i18n.t('missing.label', { defaultValue: '备用文案' }), '备用文案');
});

test('translations interpolate zero counts and text values', () => {
  assert.equal(i18n.t('ruleSets.count', { count: 0 }), '0 条');
  assert.equal(i18n.t('ruleSets.count', { count: 3 }), '3 条');
  assert.equal(i18n.t('rules.example', { text: '香港/A & B' }), '示例:香港/A & B');
});

test('useTranslation renders initialized Chinese labels and interpolated text', () => {
  function Labels() {
    const { t, ready } = useTranslation();
    assert.equal(ready, true);
    return React.createElement('section', null,
      React.createElement('h1', null, t('login.title')),
      React.createElement('p', null, t('rules.example', { text: '香港/A & B' })));
  }

  assert.equal(renderToStaticMarkup(React.createElement(Labels)),
    '<section><h1>管理员登录</h1><p>示例:香港/A &amp; B</p></section>');
});
