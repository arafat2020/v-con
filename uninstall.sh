#!/usr/bin/env bash
# Remove a vcon install created by install.sh.
#
#   ./uninstall.sh                         # ~/.local, keep inventory and logs
#   sudo ./uninstall.sh --prefix /usr/local
#   ./uninstall.sh --purge                 # also delete config, state, and cache
#   ./uninstall.sh -y                      # do not ask
set -euo pipefail

YES=0
PURGE=0
PREFIX="${PREFIX:-}"

usage() {
    cat <<'EOF'
Usage: ./uninstall.sh [--prefix DIR] [--purge] [-y]

Remove the vcon binary, man page, and shell completions recorded by
install.sh. Inventory, snippets, audit logs, and state stay unless you
pass --purge.

  --prefix DIR   Only uninstall this prefix (default: look for a manifest)
  --purge        Also delete vcon config, data, state, and cache
  -y, --yes      Do not ask for confirmation
  -h, --help     Show this help
EOF
}

die() {
    printf 'uninstall.sh: %s\n' "$*" >&2
    exit 1
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
        --purge)
            PURGE=1
            shift
            ;;
        -y | --yes)
            YES=1
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

user_home() {
    if [[ -n "${SUDO_USER:-}" && "${SUDO_USER}" != "root" ]]; then
        # sudo's HOME is root's. Purge the account that invoked sudo.
        if [[ "${SUDO_USER}" =~ ^[A-Za-z0-9._-]+$ ]]; then
            (cd ~"${SUDO_USER}" && pwd)
            return
        fi
    fi
    printf '%s\n' "$HOME"
}

manifest_for_prefix() {
    local prefix="${1%/}"
    printf '%s\n' "${prefix}/share/vcon/installed.manifest"
}

discover_manifests() {
    local found=()
    local candidate home
    if [[ -n "$PREFIX" ]]; then
        candidate="$(manifest_for_prefix "$PREFIX")"
        [[ -f "$candidate" ]] || die "no install manifest at ${candidate}. Nothing to remove."
        printf '%s\n' "$candidate"
        return
    fi
    home="$(user_home)"
    for candidate in \
        "${home}/.local/share/vcon/installed.manifest" \
        "/usr/local/share/vcon/installed.manifest"; do
        if [[ -f "$candidate" ]]; then
            found+=("$candidate")
        fi
    done
    if [[ ${#found[@]} -eq 0 ]]; then
        printf 'vcon does not look installed (no manifest under ~/.local or /usr/local).\n' >&2
        printf 'If you used another prefix: ./uninstall.sh --prefix DIR\n' >&2
        exit 0
    fi
    printf '%s\n' "${found[@]}"
}

writable_prefix() {
    local prefix="$1"
    local probe="${prefix}/.vcon-uninstall-probe"
    if [[ -w "$prefix" ]] && touch "$probe" 2>/dev/null; then
        rm -f "$probe"
        return 0
    fi
    return 1
}

# Prints files from one manifest, one per line. First line of the file is prefix=.
files_in_manifest() {
    local manifest="$1"
    local line
    while IFS= read -r line || [[ -n "$line" ]]; do
        [[ -z "$line" || "$line" == \#* || "$line" == prefix=* ]] && continue
        printf '%s\n' "$line"
    done <"$manifest"
}

prefix_in_manifest() {
    local manifest="$1"
    local line
    line="$(grep -E '^prefix=' "$manifest" | head -n 1 || true)"
    printf '%s\n' "${line#prefix=}"
}

if ! discovered="$(discover_manifests)"; then
    exit 1
fi
MANIFESTS=()
while IFS= read -r manifest; do
    [[ -n "$manifest" ]] && MANIFESTS+=("$manifest")
done <<<"$discovered"

if [[ ${#MANIFESTS[@]} -eq 0 ]]; then
    exit 0
fi

printf 'This will remove:\n'
for manifest in "${MANIFESTS[@]}"; do
    prefix="$(prefix_in_manifest "$manifest")"
    printf '\n  from %s\n' "${prefix:-unknown prefix}"
    while IFS= read -r path; do
        printf '    %s\n' "$path"
    done < <(files_in_manifest "$manifest")
done

if [[ "$PURGE" -eq 1 ]]; then
    HOME_DIR="$(user_home)"
    CONFIG_DIR="${XDG_CONFIG_HOME:-${HOME_DIR}/.config}/vcon"
    DATA_DIR="${XDG_DATA_HOME:-${HOME_DIR}/.local/share}/vcon"
    STATE_DIR="${XDG_STATE_HOME:-${HOME_DIR}/.local/state}/vcon"
    CACHE_DIR="${XDG_CACHE_HOME:-${HOME_DIR}/.cache}/vcon"
    cat <<EOF

--purge also deletes user data:
  ${CONFIG_DIR}
  ${DATA_DIR}
  ${STATE_DIR}
  ${CACHE_DIR}
EOF
else
    printf '\nInventory and logs are kept. Pass --purge to delete them too.\n'
fi

if [[ "$YES" -eq 0 ]]; then
    if [[ ! -r /dev/tty ]]; then
        die "no terminal to confirm. Re-run with -y"
    fi
    printf '\nRemove these files? [y/N] '
    # /dev/tty so `curl … | bash` does not read the prompt from the pipe.
    read -r answer </dev/tty
    case "$answer" in
        y | Y | yes | YES) ;;
        *)
            printf 'Cancelled.\n'
            exit 0
            ;;
    esac
fi

removed=0
skipped=0
processed=0
for manifest in "${MANIFESTS[@]}"; do
    prefix="$(prefix_in_manifest "$manifest")"
    [[ -n "$prefix" ]] || die "manifest ${manifest} has no prefix= line"
    if ! writable_prefix "$prefix"; then
        printf 'skip %s (not writable; try sudo ./uninstall.sh --prefix %s)\n' "$prefix" "$prefix" >&2
        skipped=$((skipped + 1))
        continue
    fi
    processed=1
    while IFS= read -r path; do
        # Only delete paths recorded under this prefix.
        case "$path" in
            "$prefix"/*)
                if [[ -e "$path" || -L "$path" ]]; then
                    rm -f "$path"
                    removed=$((removed + 1))
                fi
                ;;
            *)
                printf 'skip %s (outside %s)\n' "$path" "$prefix" >&2
                ;;
        esac
    done < <(files_in_manifest "$manifest")
    rm -f "$manifest"
    rmdir "${prefix}/share/vcon" 2>/dev/null || true
    rmdir "${prefix}/share/man/man1" 2>/dev/null || true
    rmdir "${prefix}/share/bash-completion/completions" 2>/dev/null || true
    rmdir "${prefix}/share/zsh/site-functions" 2>/dev/null || true
    rmdir "${prefix}/share/fish/vendor_completions.d" 2>/dev/null || true
done

if [[ "$PURGE" -eq 1 && "$processed" -eq 1 ]]; then
    HOME_DIR="$(user_home)"
    for dir in \
        "${XDG_CONFIG_HOME:-${HOME_DIR}/.config}/vcon" \
        "${XDG_DATA_HOME:-${HOME_DIR}/.local/share}/vcon" \
        "${XDG_STATE_HOME:-${HOME_DIR}/.local/state}/vcon" \
        "${XDG_CACHE_HOME:-${HOME_DIR}/.cache}/vcon"; do
        if [[ -d "$dir" ]]; then
            rm -rf "$dir"
            printf 'removed %s\n' "$dir"
        fi
    done
fi

if [[ "$skipped" -gt 0 && "$removed" -eq 0 ]]; then
    exit 1
fi
printf 'Uninstalled vcon.\n'
