# Окружение разработки ZeroAIMemory. Запуск: source scripts/dev-env.sh
_zam_root="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
export RUSTUP_HOME="$_zam_root/.toolchain/rustup"
export CARGO_HOME="$_zam_root/.toolchain/cargo"
export PATH="$CARGO_HOME/bin:$PATH"
mkdir -p "$_zam_root/.tmp/systmp" "$_zam_root/.tmp/npm-cache"
export TMP="$_zam_root/.tmp/systmp"
export TEMP="$TMP"
export TMPDIR="$TMP"
export npm_config_cache="$_zam_root/.tmp/npm-cache"
unset _zam_root
