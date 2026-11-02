// Политика лицензий бандла WebUI (проект под MIT): список ALLOWED + точечное исключение.
// Выделено из vite.config.ts, чтобы проверяться модульным тестом (license-policy.test.ts).
/** Подмножество Dependency из rollup-plugin-license. */
export interface PackageInfo {
  name?: string | null;
  license?: string | null;
  licenseText?: string | null;
}

/**
 * Разрешённые лицензии. Zlib (M5) — у pako 1.x (`(MIT AND Zlib)`, распаковка zip в jszip
 * просмотрщика DOCX): пермиссивная, совместима с MIT, требует лишь сохранять уведомление.
 */
export const ALLOWED = new Set(['MIT', 'ISC', 'BSD-2-Clause', 'BSD-3-Clause', 'Apache-2.0', '0BSD', 'Zlib']);

/**
 * Лицензии, допустимые только конкретным пакетам (решение владельца 2026-10-21):
 * elkjs (EPL-2.0) — раскладка ELK для Mermaid в отдельном ленивом чанке; текст лицензии и
 * ссылка на исходники — в THIRD-PARTY-NOTICES.txt (EPL-2.0 §3).
 */
export const ALLOWED_FOR_PACKAGE: Record<string, string> = { elkjs: 'EPL-2.0' };

// Лицензия пакета; если поле license в package.json не задано (svelte-toolbelt, khroma),
// она определяется по первой строке файла LICENSE (MIT или ISC).
export function licenseOf(d: PackageInfo): string | null {
  if (d.license) return d.license;
  const head = d.licenseText?.trimStart() ?? '';
  if (/^(The )?MIT License/.test(head)) return 'MIT';
  if (/^ISC License/.test(head)) return 'ISC';
  return null;
}

// Выражение SPDX вида `(A OR B)` допустимо, если допустима хотя бы одна альтернатива
// (DOMPurify: `(MPL-2.0 OR Apache-2.0)` — по Apache-2.0; jszip: `(MIT OR GPL-3.0-or-later)` —
// по MIT); `(A AND B)` — если допустимы все части (pako: `(MIT AND Zlib)`). Скобки внутри
// выражения и смешение OR с AND не разбираются: такая лицензия отклоняется.
function listed(license: string | null): boolean {
  if (!license) return false;
  const expr = license.trim().replace(/^\((.*)\)$/, '$1');
  if (/[()]/.test(expr)) return false;
  const or = expr.split(/\s+OR\s+/);
  const and = expr.split(/\s+AND\s+/);
  if (or.length > 1 && and.length > 1) return false;
  if (and.length > 1) return and.every((l) => ALLOWED.has(l.trim()));
  return or.some((l) => ALLOWED.has(l.trim()));
}

/** Допустим ли пакет в бандле: общий список или точечное исключение по имени И лицензии. */
export function allowed(d: PackageInfo): boolean {
  const lic = licenseOf(d);
  if (listed(lic)) return true;
  return !!d.name && Object.hasOwn(ALLOWED_FOR_PACKAGE, d.name) && ALLOWED_FOR_PACKAGE[d.name] === lic;
}
