const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { test } = require('node:test');
const { webRequire } = require('./helpers.cjs');
const { ESLint } = webRequire('eslint');
const web = path.resolve(__dirname, '../../web');

test('lint file patterns include nested TypeScript sources and ignore build output', async () => {
  const eslint = new ESLint({ cwd: web });
  const expected = fs.readdirSync(path.join(web, 'src'), { recursive: true })
    .filter((file) => /\.tsx?$/.test(file))
    .map((file) => path.join(web, 'src', file))
    .sort();
  const results = await eslint.lintFiles(['src/**/*.{ts,tsx}']);
  assert.deepEqual(results.map((result) => result.filePath).sort(), expected);
  assert.equal(await eslint.isPathIgnored(path.join(web, 'dist/assets/example.js')), true);
});

test('nested component files retain the React hook rules', async () => {
  const eslint = new ESLint({ cwd: web });
  const results = await eslint.lintText(`
    import { useState } from 'react';
    export function Example({ enabled }: { enabled: boolean }) {
      if (enabled) useState(0);
      return null;
    }
  `, { filePath: path.join(web, 'src/components/Example.tsx') });
  assert.ok(results[0].messages.some((message) => message.ruleId === 'react-hooks/rules-of-hooks'));
});
