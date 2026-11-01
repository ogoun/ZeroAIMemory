// Пакеты, код которых попадает в бандл внутри чужой готовой сборки и поэтому не виден
// rollup-plugin-license: pako, lie, immediate, setimmediate — внутри dist/jszip.min.js (поле
// browser пакета jszip; setimmediate — полифилл, его ветка `new Function` для строкового кода
// недостижима: jszip передаёт функции, и CSP WebUI без 'unsafe-eval' её бы запретила). Их
// лицензии проверяются той же политикой (license-policy.ts) и попадают в THIRD-PARTY-NOTICES.
import { readdirSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

/** Встроенный пакет и пакет, в сборку которого он входит. */
export const EMBEDDED = [
  { name: 'pako', via: 'jszip' },
  { name: 'lie', via: 'jszip' },
  { name: 'immediate', via: 'jszip' },
  { name: 'setimmediate', via: 'jszip' },
];

/** Сведения о встроенных пакетах в форме Dependency rollup-plugin-license. */
export function embeddedDeps() {
  return EMBEDDED.map(({ name, via }) => {
    const dir = fileURLToPath(new URL(`../node_modules/${name}/`, import.meta.url));
    const pkg = JSON.parse(readFileSync(`${dir}package.json`, 'utf8'));
    return {
      name,
      version: `${pkg.version} (в составе ${via})`,
      license: pkg.license ?? null,
      // имя файла лицензии у пакетов разное: LICENSE, LICENSE.txt, license.md
      licenseText: readFileSync(`${dir}${readdirSync(dir).find((f) => /^licen[cs]e/i.test(f))}`, 'utf8'),
      repository: pkg.repository ?? null,
    };
  });
}
