---
tags: [playwright, e2e, testing]
---
# Playwright: waiting, traces and taming flaky end-to-end tests

Our end-to-end suite (about 300 specs) went from "re-run until green" to a failure rate below
0.5 % per run. This note collects what actually made the difference. Most of it boils down to
two ideas: let Playwright do the waiting, and record enough evidence to debug a failure that
happened once on a CI machine.

## Auto-waiting and actionability

Every action on a locator (`click`, `fill`, `check`…) waits until the element is attached,
visible, stable (not animating), enabled and receives events (not covered by an overlay).
Only then the action runs. If the conditions are not met within the action timeout, the
test fails with a log of which check was pending.

So the first rule: **no manual sleeps**.

```ts
// bad: arbitrary delay, slow on fast machines, still flaky on slow ones
await page.waitForTimeout(2000);
await page.click("#save");

// good: the locator waits for actionability
await page.getByRole("button", { name: "Save" }).click();
```

`page.waitForTimeout` exists for debugging only. We ban it with a lint rule
(`playwright/no-wait-for-timeout` from `eslint-plugin-playwright`).

## Web-first assertions

Assertions on locators retry until they pass or time out (default 5 s):

```ts
await expect(page.getByText("Saved")).toBeVisible();
await expect(page.getByRole("row")).toHaveCount(3);
await expect(page).toHaveURL(/\/orders\/\d+/);
```

The classic flaky pattern is a one-shot read followed by a plain assertion:

```ts
// flaky: isVisible() returns immediately, no retry
expect(await page.getByText("Saved").isVisible()).toBe(true);
```

Same for `textContent()` and `count()`. If the value comes from a locator, assert on the
locator. For non-locator values that eventually change, use `expect.poll` or `toPass`:

```ts
await expect.poll(() => api.getJobStatus(id), { timeout: 30_000 }).toBe("done");
```

## Locator choice

| Locator | Stability |
|---------|-----------|
| `getByRole("button", { name })` | Best: tied to what the user sees and to accessibility |
| `getByLabel`, `getByPlaceholder` | Good for forms |
| `getByTestId("checkout")` | Stable, explicit contract with developers |
| CSS like `.btn-primary:nth-child(2)` | Breaks on any layout change |
| XPath | Avoid |

Strictness helps: a locator that matches two elements throws "strict mode violation" instead
of clicking the first one silently. Fix the locator rather than adding `.first()` everywhere.

## Traces

The trace viewer is the single most valuable debugging tool. Config we use:

```ts
// playwright.config.ts
export default defineConfig({
  retries: process.env.CI ? 2 : 0,
  use: {
    trace: "on-first-retry",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
  },
  reporter: [["html", { open: "never" }], ["github"]],
});
```

`on-first-retry` records a trace only when a test is retried, so passing runs cost nothing.
A trace contains the action log, DOM snapshots before and after every action, network
requests, console messages and the source line. Open it locally:

```bash
npx playwright show-trace test-results/checkout-chromium-retry1/trace.zip
```

or drag the zip into trace.playwright.dev (processed in the browser, nothing uploaded). We
upload `test-results/` and `playwright-report/` as CI artifacts with `if: ${{ !cancelled() }}`
so they survive failed jobs.

## Retries are a signal, not a fix

With `retries: 2`, a test that fails and then passes is reported as **flaky** in the HTML
report. We treat that list as a backlog: each flaky test gets an owner. Retries only keep
the main branch usable while the cause is investigated.

Common causes we found, roughly by frequency:

1. **Shared state between tests.** Tests reused one account; parallel workers changed the
   same cart. Fix: create data per test through the API in a fixture, use unique names.
2. **Assertions racing the network.** Clicking "Save" and immediately navigating away. Fix:
   assert on the result (toast, URL) or `await page.waitForResponse(...)` started *before*
   the click.
3. **Animations and transitions.** Use `reducedMotion: "reduce"` in `use`, and disable CSS
   transitions in a test-only stylesheet.
4. **Time and timezones.** Tests asserting "today" failing near midnight UTC. Fix: `timezoneId`
   in config and the Clock API (`page.clock.setFixedTime`).
5. **Third-party scripts.** Analytics and chat widgets loading slowly and covering buttons.
   Block them with `page.route("**/analytics/**", r => r.abort())`.
6. **Too many workers on a small CI machine.** CPU starvation makes everything slower than
   the timeouts. We use `workers: process.env.CI ? 2 : undefined`.

## Hunting a flaky test locally

```bash
npx playwright test checkout.spec.ts --repeat-each=50 --workers=4
npx playwright test -g "applies coupon" --repeat-each=100 --retries=0
```

`--repeat-each` multiplies runs of the selected tests; combined with more workers it often
reproduces timing problems that only appear under load. `--debug` opens the inspector and
`--ui` gives the watch mode with time-travel through the trace.

## Mocking the network where it does not matter

End-to-end does not have to mean "every request hits a real backend". For screens whose
backend is covered elsewhere, we stub responses with `page.route`:

```ts
await page.route("**/api/recommendations", route =>
  route.fulfill({ json: [{ id: 1, title: "Fixture item" }] }),
);
```

The test becomes deterministic and fast, and it no longer fails when a shared staging
service is down. We keep a small set of "full stack" specs (login, checkout, payment
callback) that run against a real environment, and stub the rest. For HAR-based replay,
`page.routeFromHAR("fixtures/catalog.har", { update: false })` serves recorded responses;
re-record with `update: true` when the API contract changes.

## Sharding in CI

A single runner took 40 minutes for the full suite. Playwright splits the suite itself:

```bash
npx playwright test --shard=1/4
```

Each matrix job runs one shard and uploads a blob report (`reporter: "blob"`); a final job
downloads them and runs `npx playwright merge-reports --reporter html ./all-blob-reports` to
produce one HTML report. Set `fullyParallel: true` so shards are balanced by test, not by file.

## Authentication without flakiness

Logging in through the UI in every test is slow and fragile. A setup project logs in once and
saves `storageState`:

```ts
projects: [
  { name: "setup", testMatch: /auth\.setup\.ts/ },
  { name: "chromium", use: { storageState: ".auth/user.json" }, dependencies: ["setup"] },
]
```

Keep `.auth/` out of version control.

## Summary rules

- No `waitForTimeout`; rely on actionability and web-first assertions.
- Role-based or test-id locators; no positional CSS.
- Isolated data per test.
- Trace on first retry, artifacts uploaded always.
- Every flaky report in the HTML report is a ticket with an owner.
