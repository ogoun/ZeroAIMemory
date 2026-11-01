// Пакеты, код которых попадает в бандл внутри чужой готовой сборки и поэтому не виден
// rollup-plugin-license: pako — внутри dist/jszip.min.js (поле browser пакета jszip). Их
// лицензии проверяются той же политикой (license-policy.ts) и попадают в THIRD-PARTY-NOTICES.
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

/** Встроенный пакет и пакет, в сборку которого он входит. */
export const EMBEDDED = [{ name: 'pako', via: 'jszip' }];

/** Сведения о встроенных пакетах в форме Dependency rollup-plugin-license. */
export function embeddedDeps() {
  return EMBEDDED.map(({ name, via }) => {
    const dir = fileURLToPath(new URL(`../node_modules/${name}/`, import.meta.url));
    const pkg = JSON.parse(readFileSync(`${dir}package.json`, 'utf8'));
    return {
      name,
      version: `${pkg.version} (в составе ${via})`,
      license: pkg.license ?? null,
      licenseText: readFileSync(`${dir}LICENSE`, 'utf8'),
      repository: pkg.repository ?? null,
    };
  });
}
