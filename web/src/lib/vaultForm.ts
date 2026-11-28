// Поля формы записи Vault (M7.6, решение 167): типы записей и открытые поля `data` — общие у
// страницы Vault и окна «Подключение модели» (`VaultForm.svelte`). Значение секрета сюда не
// попадает: оно живёт только в поле формы до отправки.
import type { VaultData } from './api';

/** Допустимые `data.type` (как `VAULT_TYPES` сервера, `api/vault.rs`). */
export const VAULT_TYPES = ['password', 'token', 'ssh_key', 'connection_string', 'url', 'repo', 'note', 'host', 'environment'] as const;

/** Тип записи — по-русски. */
export const VAULT_TYPE_TEXT: Record<string, string> = {
  password: 'пароль',
  token: 'токен',
  ssh_key: 'SSH-ключ',
  connection_string: 'строка подключения',
  url: 'адрес',
  repo: 'репозиторий',
  note: 'заметка',
  host: 'хост',
  environment: 'окружение',
};

/** Протокол подключения модели (как `Protocol` страницы «Внутренние работы»). */
export type ModelProtocol = 'openai' | 'anthropic';

/** Подсказка поля «Адрес API» записи модели по протоколу подключения. */
export const MODEL_URL_HINT: Record<ModelProtocol, string> = {
  openai: 'адрес API с /v1, например https://api.openai.com/v1 или http://адрес-машины:8080/v1',
  anthropic: 'https://api.anthropic.com (с /v1 на конце или без)',
};

/** Открытые поля формы — строки (пусто — поля нет). */
export interface VaultFields {
  url: string;
  username: string;
  description: string;
  purpose: string;
  address: string;
  limits: string;
  notes: string;
}

/** Поля формы из `data` записи (нет поля — пустая строка). */
export function vaultFieldsOf(d: VaultData | null | undefined): VaultFields {
  return {
    url: d?.url ?? '',
    username: d?.username ?? '',
    description: d?.description ?? '',
    purpose: d?.purpose ?? '',
    address: d?.address ?? '',
    limits: d?.limits ?? '',
    notes: d?.notes ?? '',
  };
}
