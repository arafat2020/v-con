#!/usr/bin/env bash
# Build vcon and install the binary, man page, and shell completions.
#
# One line (after this file is on the default branch):
#   curl -fsSL https://raw.githubusercontent.com/arafat2020/v-con/main/install.sh | bash
#
# From a checkout:
#   ./install.sh                         # ~/.local  (or /usr/local as root)
#   sudo ./install.sh --prefix /usr/local
#   ./install.sh --skip-build            # reuse target/release/vcon
set -euo pipefail

REPO_URL="https://github.com/arafat2020/v-con.git"
REPO_REF="${VCON_REF:-main}"

die() {
    printf 'install.sh: %s\n' "$*" >&2
    exit 1
}

# `curl … | bash` has no checkout beside the script. Clone, then run that copy.
# A normal ./install.sh already sits next to Cargo.toml and skips this.
if [[ ! -f "${BASH_SOURCE[0]:-}" ]] || [[ ! -f "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/Cargo.toml" ]]; then
    command -v git >/dev/null 2>&1 || die "git is required to install from a URL"
    checkout="$(mktemp -d)"
    trap 'rm -rf "$checkout"' EXIT
    printf 'Cloning %s (%s)...\n' "$REPO_URL" "$REPO_REF"
    git clone --depth 1 --branch "$REPO_REF" "$REPO_URL" "${checkout}/vcon"
    set +e
    bash "${checkout}/vcon/install.sh" "$@"
    status=$?
    set -e
    rm -rf "$checkout"
    exit "$status"
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SKIP_BUILD=0
PREFIX="${PREFIX:-}"

usage() {
    cat <<'EOF'
Usage: ./install.sh [--prefix DIR] [--skip-build]

Build the release binary and install:

  DIR/bin/vcon
  DIR/share/man/man1/vcon.1
  DIR/share/bash-completion/completions/vcon
  DIR/share/zsh/site-functions/_vcon
  DIR/share/fish/vendor_completions.d/vcon.fish

DIR defaults to ~/.local, or /usr/local when the script runs as root.
Your inventory and settings (~/.config/vcon) are not touched.

  --prefix DIR   Install under DIR instead of the default
  --skip-build   Install target/release/vcon without compiling
  -h, --help     Show this help

From a URL:

  curl -fsSL https://raw.githubusercontent.com/arafat2020/v-con/main/install.sh | bash
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --prefix)
            [[ $# -ge 2 ]] || die "--prefix needs a directory"
            PREFIX="$2"
            shift 2
            ;;
        --prefix=*)
            PREFIX="${1#*=}"
            [[ -n "$PREFIX" ]] || die "--prefix needs a directory"
            shift
            ;;
        --skip-build)
            SKIP_BUILD=1
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            die "unknown argument: $1 (try --help)"
            ;;
    esac
done

if [[ -z "$PREFIX" ]]; then
    if [[ "$(id -u)" -eq 0 ]]; then
        PREFIX="/usr/local"
    else
        PREFIX="${HOME}/.local"
    fi
fi
PREFIX="${PREFIX%/}"

command -v cargo >/dev/null 2>&1 || die "cargo is not on PATH. Install Rust from https://rustup.rs"
command -v ssh >/dev/null 2>&1 || printf 'warning: ssh is not on PATH. vcon needs it to connect.\n' >&2

SRC="${ROOT}/target/release/vcon"
if [[ "$SKIP_BUILD" -eq 0 ]]; then
    printf 'Building release binary (first run fetches crates and can take several minutes)...\n'
    cargo build --release --manifest-path "${ROOT}/Cargo.toml"
else
    [[ -x "$SRC" ]] || die "no binary at ${SRC}. Run without --skip-build."
fi
[[ -x "$SRC" ]] || die "build finished but ${SRC} is missing"

if ! mkdir -p "$PREFIX" 2>/dev/null; then
    die "cannot create ${PREFIX}. Re-run with sudo, or choose a directory you can write: ./install.sh --prefix \"\$HOME/.local\""
fi
PREFIX="$(cd "$PREFIX" && pwd)"

BIN_DIR="${PREFIX}/bin"
MAN_DIR="${PREFIX}/share/man/man1"
BASH_DIR="${PREFIX}/share/bash-completion/completions"
ZSH_DIR="${PREFIX}/share/zsh/site-functions"
FISH_DIR="${PREFIX}/share/fish/vendor_completions.d"
META_DIR="${PREFIX}/share/vcon"
MANIFEST="${META_DIR}/installed.manifest"

mkdir -p "$BIN_DIR" "$MAN_DIR" "$BASH_DIR" "$ZSH_DIR" "$FISH_DIR" "$META_DIR"

install -m 755 "$SRC" "${BIN_DIR}/vcon"
BIN="${BIN_DIR}/vcon"

write_generated() {
    local dest="$1"
    shift
    local tmp
    tmp="$(mktemp)"
    if ! "$BIN" "$@" >"$tmp"; then
        rm -f "$tmp"
        die "could not run: vcon $*"
    fi
    install -m 644 "$tmp" "$dest"
    rm -f "$tmp"
}

printf 'Installing completions and man page...\n'
write_generated "${BASH_DIR}/vcon" completions bash
write_generated "${ZSH_DIR}/_vcon" completions zsh
write_generated "${FISH_DIR}/vcon.fish" completions fish
write_generated "${MAN_DIR}/vcon.1" man

{
    printf 'prefix=%s\n' "$PREFIX"
    printf '%s\n' \
        "${BIN_DIR}/vcon" \
        "${MAN_DIR}/vcon.1" \
        "${BASH_DIR}/vcon" \
        "${ZSH_DIR}/_vcon" \
        "${FISH_DIR}/vcon.fish"
} >"$MANIFEST"

printf '\nInstalled vcon to %s\n' "$BIN"

case ":${PATH}:" in
    *":${BIN_DIR}:"*) ;;
    *)
        printf '\n%s is not on PATH. Add this to your shell profile:\n\n  export PATH="%s:$PATH"\n' "$BIN_DIR" "$BIN_DIR"
        ;;
esac

case "$PREFIX" in
    "${HOME}"/*)
    cat <<EOF

User install notes:

  zsh completions are not picked up until this directory is on fpath
  (put it in ~/.zshrc, then open a new shell):

    fpath=(${ZSH_DIR} \$fpath)
    autoload -Uz compinit && compinit

  man may not search ~/.local until MANPATH includes it:

    export MANPATH="${PREFIX}/share/man:\${MANPATH:-}"

EOF
        ;;
esac

printf 'Next: vcon doctor && vcon init\n'
