// Подставная модель для e2e (M7.3, спецификация §6.9 «Проверка», решение 65): OpenAI-совместимый
// Chat Completions на `127.0.0.1` и свободном порту — release-сборка `zam-server` без `test-hooks`
// обращается к нему только при `ZAM_WORKS__ALLOW_LOOPBACK=true` (global-setup). Путь — любой с
// хвостом `/chat/completions` под каким-нибудь префиксом: смена адреса записи Vault (`/v1` → `/v2`)
// ведёт на тот же сервер, а адрес без пути (`<корень>/chat/completions`) — 404, как у настоящих
// серверов с API под `/v1` (M7.6, решение 168). Проверка подключения (системное сообщение
// «проверка подключения») получает `{"ok": true}` с меткой ответа, пакет работы — `{"items": []}`
// (элементы без итога). Сценарии читают принятые запросы `GET /__requests` (путь, модель, был ли
// ключ, протокол, вид) — и только здесь, в подставном сервере, есть текст запроса и ответа: журнал
// отправок сервера его не хранит.
// M7.6 (§9.2; решение 169): `POST …/v1/messages` — Anthropic Messages (ключ — `x-api-key`), `GET
// …/models` — список моделей; модель `e2e-401` — ответ 401 (ключ не подошёл), `e2e-missing` — 404
// «модели нет» (список моделей при этом отдаётся).
import { createServer, type IncomingMessage, type Server } from 'node:http';

/** Метка ответа модели: её не должно быть ни на странице, ни в журнале отправок. */
export const MOCK_REPLY_MARK = 'e2e-ответ-модели-7d3f';
/** Модели подставного сервера (`GET …/models`). */
export const MOCK_MODELS = ['e2e-mock', 'e2e-other'];

export interface MockRequest {
  path: string;
  model: string;
  /** Пришёл ли ключ (`Authorization` или `x-api-key`). */
  auth: boolean;
  /** Протокол запроса: Chat Completions или Anthropic Messages. */
  protocol: 'openai' | 'anthropic';
  kind: 'test' | 'work';
  at: string;
}

export interface MockModel {
  /** Базовый адрес без пути: `http://127.0.0.1:<порт>`. */
  url: string;
  close: () => Promise<void>;
}

const body = (req: IncomingMessage): Promise<string> =>
  new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    req.on('data', (c: Buffer) => chunks.push(c));
    req.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')));
    req.on('error', reject);
  });

/** Запустить подставную модель на `127.0.0.1` (свободный порт). */
export async function startMockModel(): Promise<MockModel> {
  const seen: MockRequest[] = [];
  const server: Server = createServer(async (req, res) => {
    const path = (req.url ?? '/').split('?')[0];
    const send = (status: number, v: unknown) => {
      res.writeHead(status, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(v));
    };
    try {
      if (req.method === 'GET' && path === '/__requests') return send(200, seen);
      if (req.method === 'POST' && path === '/__reset') {
        seen.length = 0;
        return send(200, {});
      }
      // список моделей — под префиксом API (у корня — нет: «по адресу нет API»)
      if (req.method === 'GET' && path.endsWith('/models') && path !== '/models') {
        return send(200, { object: 'list', data: MOCK_MODELS.map((id) => ({ id, object: 'model' })) });
      }
      const anthropic = req.method === 'POST' && path.endsWith('/v1/messages');
      const chat = req.method === 'POST' && path.endsWith('/chat/completions') && path !== '/chat/completions';
      if (!anthropic && !chat) return send(404, { error: 'not found' });
      const j = JSON.parse(await body(req)) as { model?: string; system?: string; messages?: { role: string; content: string }[] };
      const system = anthropic ? (j.system ?? '') : (j.messages?.find((m) => m.role === 'system')?.content ?? '');
      const kind: MockRequest['kind'] = system.includes('проверка подключения') ? 'test' : 'work';
      const auth = anthropic ? !!req.headers['x-api-key'] : !!req.headers.authorization;
      seen.push({ path, model: j.model ?? '', auth, protocol: anthropic ? 'anthropic' : 'openai', kind, at: new Date().toISOString() });
      if (j.model === 'e2e-401') {
        return anthropic
          ? send(401, { type: 'error', error: { type: 'authentication_error', message: 'invalid x-api-key' } })
          : send(401, { error: { message: 'Incorrect API key provided', type: 'invalid_request_error' } });
      }
      if (j.model === 'e2e-missing') {
        return anthropic
          ? send(404, { type: 'error', error: { type: 'not_found_error', message: 'model: e2e-missing' } })
          : send(404, { error: { message: 'The model does not exist', code: 'model_not_found' } });
      }
      const content = kind === 'test' ? JSON.stringify({ ok: true, note: MOCK_REPLY_MARK }) : JSON.stringify({ items: [] });
      if (anthropic) {
        return send(200, {
          id: `msg_e2e_${seen.length}`,
          type: 'message',
          role: 'assistant',
          model: j.model ?? 'e2e',
          content: [{ type: 'text', text: content }],
          stop_reason: 'end_turn',
          stop_sequence: null,
          usage: { input_tokens: 40 + seen.length, cache_creation_input_tokens: 0, cache_read_input_tokens: 0, output_tokens: 7 },
        });
      }
      return send(200, {
        id: `e2e-${seen.length}`,
        object: 'chat.completion',
        model: j.model ?? 'e2e',
        choices: [{ index: 0, message: { role: 'assistant', content }, finish_reason: 'stop' }],
        usage: { prompt_tokens: 40 + seen.length, completion_tokens: 7 },
      });
    } catch (e) {
      return send(400, { error: { message: String(e), type: 'invalid_request_error' } });
    }
  });
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => resolve());
  });
  const port = (server.address() as { port: number }).port;
  return {
    url: `http://127.0.0.1:${port}`,
    // соединения keep-alive клиента сервера держали бы `close` до конца их срока
    close: () =>
      new Promise<void>((r) => {
        server.close(() => r());
        server.closeAllConnections();
      }),
  };
}
