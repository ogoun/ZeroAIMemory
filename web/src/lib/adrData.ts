// `data` insight при замещении в WebUI (ревью задачи 13a M6, m1): поля ADR сервер читает только
// из `data.decision`; ключи ADR на верхнем уровне `data` у решения — 422
// (crates/zam-server/src/api/insights.rs, DECISION_TOP_LEVEL).

/** Ключи ADR, которые сервер не принимает на верхнем уровне `data` решения. */
export const DECISION_TOP_LEVEL = ['options', 'chosen', 'rationale', 'consequences', 'revisit_if'] as const;

/**
 * `data` новой записи при замещении: прочие поля прежней записи сохраняются, вид — из формы,
 * `decision` — ADR формы (нет — ключ убирается). У решения ключи ADR верхнего уровня прежней
 * записи не копируются: сервер их как ADR никогда не читал (форма показывает только
 * `data.decision`), а перенос в `data.decision` дал бы решение, которого человек в форме не видел
 * и не подтверждал (и мог бы не пройти проверку типов).
 */
export function supersedeData(
  old: Record<string, unknown>,
  kind: string,
  decision: Record<string, unknown> | null,
): Record<string, unknown> {
  const data: Record<string, unknown> = { ...old, kind };
  if (kind === 'decision') for (const k of DECISION_TOP_LEVEL) delete data[k];
  if (decision) data.decision = decision;
  else delete data.decision;
  return data;
}
