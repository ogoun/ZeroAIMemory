// Общая веб-сессия администратора для сценариев, которые не проверяют сам вход:
// вход по паролю ограничен 5 попытками в минуту на IP (ТЗ §11), поэтому global-setup
// входит один раз и сохраняет cookie в состояние браузера Playwright.
import { fileURLToPath } from 'node:url';

export const ADMIN_STATE = fileURLToPath(new URL('../../.tmp/e2e/admin-state.json', import.meta.url));
