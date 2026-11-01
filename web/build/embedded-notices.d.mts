/** Пакеты внутри чужих готовых сборок (см. embedded-notices.mjs). */
export const EMBEDDED: { name: string; via: string }[];

export function embeddedDeps(): {
  name: string;
  version: string;
  license: string | null;
  licenseText: string;
  repository: string | { url?: string } | null;
}[];
