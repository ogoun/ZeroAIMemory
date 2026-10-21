# winget: reproducible dev machine setup

`winget` ships with modern Windows 10/11 as part of App Installer. I use it to rebuild a
workstation from a list instead of clicking through installers.

```powershell
winget search ripgrep
winget install --id BurntSushi.ripgrep.MSVC -e --source winget
winget upgrade --all --include-unknown
winget list --upgrade-available
```

- Always install by `--id` with `-e` (exact). Name matching is fuzzy and can pick the wrong
  package when several publishers ship similarly named tools.
- `--source winget` avoids accidentally pulling from the Microsoft Store source.
- `--scope user` installs per user when the package supports it, no admin prompt.

Export and import a machine's package list:

```powershell
winget export -o apps.json
winget import -i apps.json --accept-package-agreements --accept-source-agreements
```

The export only contains packages winget can map to a known source; anything installed
manually is listed as a warning and skipped. Version pinning: `winget pin add --id Git.Git`
keeps `upgrade --all` from touching it.
