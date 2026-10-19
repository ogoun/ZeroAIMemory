# ESLint vs Prettier: stop the formatting war

Symptom: save the file, Prettier reformats it, ESLint `--fix` on commit reformats it back,
and CI complains about `indent` or `quotes` rules. Two programs owning the same job.

Split responsibilities: Prettier formats, ESLint finds bugs.

```bash
npm i -D prettier eslint-config-prettier
```

With flat config (`eslint.config.js`) put the prettier config **last**, so it switches off
every rule that would disagree with Prettier:

```js
import js from "@eslint/js";
import prettier from "eslint-config-prettier";
export default [js.configs.recommended, prettier];
```

Notes:

- Avoid `eslint-plugin-prettier` (runs Prettier as a lint rule); it is slow and floods the
  editor with red squiggles for spacing. Run `prettier --check .` as a separate CI step.
- `npx eslint-config-prettier src/index.ts` lists enabled rules that conflict.
- In VS Code set Prettier as the default formatter and drop `source.fixAll.eslint` for
  formatting-only rules.
