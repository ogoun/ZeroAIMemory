---
tags: [typescript, tsconfig]
---
# TypeScript: strict and noUncheckedIndexedAccess

## What strict turns on

`"strict": true` is an umbrella flag. It currently enables `strictNullChecks`,
`noImplicitAny`, `strictFunctionTypes`, `strictBindCallApply`, `strictPropertyInitialization`,
`noImplicitThis`, `alwaysStrict`, `useUnknownInCatchVariables` and (since 5.6)
`strictBuiltinIteratorReturn`. New compiler versions may
add flags to the umbrella, so a TypeScript upgrade can produce new errors with no code change.
That is intentional; pin the compiler version in the lockfile and upgrade in a separate PR.

## noUncheckedIndexedAccess is not part of strict

```ts
const ports: Record<string, number> = {};
const p = ports["api"];        // number under strict
p.toFixed();                   // crashes at runtime when the key is absent

const xs = [1, 2, 3];
const first = xs[0];           // number
```

With `"noUncheckedIndexedAccess": true` both reads become `number | undefined`, so the
compiler forces a check:

```ts
const p = ports["api"];
if (p === undefined) throw new Error("api port not configured");
```

It applies to index signatures and array element access, not to declared properties or to
tuples with known length. `for (const x of xs)` and `xs.map` are unaffected, only `xs[i]`.

## Migrating an existing codebase

Turning it on in our 80k-line app produced about 1400 errors. What worked:

1. Enable it in a separate `tsconfig.strict-index.json` that extends the base and run it in
   CI as a non-blocking job to track the count.
2. Fix module by module. Most fixes are real: `arr[0]` on possibly empty arrays, `split()`
   results indexed without checks, environment lookups.
3. Prefer narrowing (`if (!x) …`, `.at(0) ?? fallback`) over the non-null assertion `!`. Lint
   with `@typescript-eslint/no-non-null-assertion` so `!` does not spread as a shortcut.
4. Flip it in the base config when the count hits zero.

## Related flags worth enabling

- `exactOptionalPropertyTypes`: `{ a?: string }` no longer accepts explicit `undefined`.
- `noImplicitOverride`: requires the `override` keyword.
- `noPropertyAccessFromIndexSignature`: forces `obj["key"]` for index-signature access,
  making the unchecked access visible in code.
