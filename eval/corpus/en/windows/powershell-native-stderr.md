---
tags: [powershell, windows]
---
# PowerShell 5.1: NativeCommandError when redirecting stderr with 2>&1

## Symptom

A build script runs fine interactively, but in CI it fails right after a perfectly
successful `git fetch`:

```
git : From https://example.com/repo
At C:\ci\build.ps1:12 char:1
+ git fetch origin 2>&1
    + CategoryInfo          : NotSpecified: (From https://example.com/repo:String) [], RemoteException
    + FullyQualifiedErrorId : NativeCommandError
```

Git exited with code 0. The "error" is just its progress output.

## Why it happens

Many native tools write informational text to stderr: git progress, cargo's
`Compiling ...` lines, curl's transfer meter. In Windows PowerShell 5.1, when you redirect
a native program's stderr into the success stream (`2>&1`) or capture it, every stderr
line is wrapped in an `ErrorRecord` with the id `NativeCommandError`. Consequences:

- `$?` becomes `$false` even though the exit code is 0;
- the records land in `$Error`;
- with `$ErrorActionPreference = 'Stop'` the first stderr line becomes a terminating
  error and the script aborts.

Without the redirection, stderr goes straight to the console and none of this happens,
which is why the script "works on my machine".

PowerShell 7.2 and later stopped treating redirected native stderr as errors under
`$ErrorActionPreference`, and 7.3 added `$PSNativeCommandUseErrorActionPreference` to fail
on non-zero exit codes instead. If you can move to pwsh 7, most of this goes away.

## Workarounds for 5.1

1. Judge success by the exit code, not by stderr:

   ```powershell
   git fetch origin
   if ($LASTEXITCODE -ne 0) { throw "git fetch failed: $LASTEXITCODE" }
   ```

2. If you need the combined output as text, let cmd do the redirection:

   ```powershell
   $out = cmd /c "git fetch origin 2>&1"
   ```

3. Locally relax the preference around the call:

   ```powershell
   $old = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
   $out = git fetch origin 2>&1 | ForEach-Object { "$_" }
   $ErrorActionPreference = $old
   ```

   The `"$_"` turns `ErrorRecord` objects back into plain strings.

4. Ask the tool to be quiet: `git fetch --quiet`, `cargo build -q`.

## Rule of thumb

In 5.1 scripts never mix `2>&1` on native executables with `ErrorActionPreference = 'Stop'`,
and always check `$LASTEXITCODE` after native calls.
