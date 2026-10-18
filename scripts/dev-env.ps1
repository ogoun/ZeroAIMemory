# Окружение разработки ZeroAIMemory. Запуск: . scripts/dev-env.ps1
$ZamRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$env:RUSTUP_HOME = Join-Path $ZamRoot '.toolchain\rustup'
$env:CARGO_HOME = Join-Path $ZamRoot '.toolchain\cargo'
$env:PATH = (Join-Path $env:CARGO_HOME 'bin') + ';' + $env:PATH
$SysTmp = Join-Path $ZamRoot '.tmp\systmp'
New-Item -ItemType Directory -Force -Path $SysTmp | Out-Null
$env:TMP = $SysTmp
$env:TEMP = $SysTmp
$NpmCache = Join-Path $ZamRoot '.tmp\npm-cache'
New-Item -ItemType Directory -Force -Path $NpmCache | Out-Null
$env:npm_config_cache = $NpmCache
Remove-Variable ZamRoot, SysTmp, NpmCache
