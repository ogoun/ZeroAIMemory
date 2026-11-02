// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { allowed } from './license-policy.ts';

test('elkjs с EPL-2.0 допустим', () => {
  assert.equal(allowed({ name: 'elkjs', license: 'EPL-2.0' }), true);
});

test('другой пакет с EPL-2.0 отклоняется', () => {
  assert.equal(allowed({ name: 'other-lib', license: 'EPL-2.0' }), false);
});

test('elkjs с иной лицензией отклоняется', () => {
  assert.equal(allowed({ name: 'elkjs', license: 'GPL-3.0' }), false);
});

test('обычные лицензии и SPDX-выражения', () => {
  assert.equal(allowed({ name: 'a', license: 'MIT' }), true);
  assert.equal(allowed({ name: 'dompurify', license: '(MPL-2.0 OR Apache-2.0)' }), true);
  assert.equal(allowed({ name: 'b', license: 'GPL-3.0' }), false);
  assert.equal(allowed({ name: 'c', license: null, licenseText: 'MIT License\n...' }), true);
  assert.equal(allowed({ name: 'd', license: null }), false);
});

test('просмотрщики документов M5: jszip по MIT, pako — MIT и Zlib, pdf.js и docx-preview', () => {
  assert.equal(allowed({ name: 'jszip', license: '(MIT OR GPL-3.0-or-later)' }), true);
  assert.equal(allowed({ name: 'pako', license: '(MIT AND Zlib)' }), true);
  assert.equal(allowed({ name: 'pdfjs-dist', license: 'Apache-2.0' }), true);
  assert.equal(allowed({ name: 'docx-preview', license: 'Apache-2.0' }), true);
});

test('AND требует всех частей, смешанные и вложенные выражения отклоняются', () => {
  assert.equal(allowed({ name: 'e', license: '(MIT AND GPL-3.0)' }), false);
  assert.equal(allowed({ name: 'f', license: 'MIT OR GPL-3.0 AND Zlib' }), false);
  assert.equal(allowed({ name: 'g', license: '(MIT OR (GPL-3.0 AND Zlib))' }), false);
  assert.equal(allowed({ name: 'h', license: 'GPL-3.0 OR LGPL-2.1' }), false);
});
