---
index: false
---
# Windows Terminal: profiles and command-line launch

Windows Terminal keeps its configuration in a `settings.json` (open it with Ctrl+Shift+,).
Each shell is a profile: PowerShell 7, Windows PowerShell, cmd, every WSL distribution and
Git Bash if you add it.

Profile for Git Bash that opens in the current project:

```json
{
  "name": "Git Bash",
  "commandline": "C:\\Program Files\\Git\\bin\\bash.exe -i -l",
  "icon": "C:\\Program Files\\Git\\mingw64\\share\\git\\git-for-windows.ico",
  "startingDirectory": "%USERPROFILE%\\src"
}
```

`defaultProfile` takes the profile GUID or name. For WSL profiles,
`"startingDirectory": "//wsl$/Ubuntu/home/me"` lands in the Linux home rather than
`/mnt/c/Users/...`.

Launching layouts from the command line with `wt`:

```
wt -d . -p "PowerShell" ; sp -V -p "Ubuntu" ; ft -t 0
```

This opens the current folder in PowerShell, splits vertically with Ubuntu, then focuses
the first tab. Under Settings → Startup you can also make Windows Terminal the default
terminal application, so console programs started from Explorer open in it instead of the
legacy console host.
