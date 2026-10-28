#!/bin/sh
# nib installer for Linux x86_64.
#
#   curl -fsSL https://raw.githubusercontent.com/StrandedDev/nib/main/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- --user        # into ~/.local/bin, no root
#   curl -fsSL .../install.sh | sh -s -- --uninstall   # remove a --user install
#   curl -fsSL .../install.sh | sh -s -- --purge       # ...and its config too
#
# Debian/Ubuntu get the .deb through apt, Fedora the .rpm through dnf; both put
# nib in /usr/bin. Any other distro, or --user, gets the tarball in
# ~/.local/bin plus a PATH line in the shell startup files. Every download is
# checked against SHA256SUMS. Re-running updates. Nothing is left in /tmp.

set -eu

REPO="StrandedDev/nib"
MARK="# added by nib installer"
USER_BIN="$HOME/.local/bin"
# Written literally into the rc files; expands when the shell starts.
# shellcheck disable=SC2016
PATH_LINE='case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) export PATH="$HOME/.local/bin:$PATH" ;; esac'

TMP=""

say() { printf 'nib: %s\n' "$*"; }
die() { printf 'nib: error: %s\n' "$*" >&2; exit 1; }
has() { command -v "$1" >/dev/null 2>&1; }

usage() {
    cat <<'EOF'
Usage: install.sh [--user] [--uninstall] [--purge]

  (no flags)   Install or update nib. Uses apt on Debian/Ubuntu and dnf on
               Fedora; elsewhere falls back to --user.
  --user       Install the tarball into ~/.local/bin (no root needed).
  --uninstall  Remove a --user install and its PATH lines.
  --purge      Like --uninstall, and also remove nib's config directory.
EOF
}

cleanup() {
    if [ -n "$TMP" ]; then
        rm -rf "$TMP"
    fi
}

fetch() { # fetch URL DEST
    if has curl; then
        curl -fsSL --proto '=https' --tlsv1.2 --retry 3 -o "$2" "$1"
    elif has wget; then
        wget -q --https-only -O "$2" "$1"
    else
        die "curl or wget is required"
    fi
}

sha256() { # sha256 FILE -> prints hex digest
    if has sha256sum; then
        sha256sum "$1" | cut -d ' ' -f 1
    elif has shasum; then
        shasum -a 256 "$1" | cut -d ' ' -f 1
    else
        die "sha256sum or shasum is required to verify the download"
    fi
}

latest_version() {
    fetch "https://api.github.com/repos/$REPO/releases/latest" "$TMP/latest.json" ||
        die "could not reach the GitHub API"
    v=$(sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"v\([^"]*\)".*/\1/p' "$TMP/latest.json" | head -n 1)
    [ -n "$v" ] || die "could not read the latest release version"
    printf '%s\n' "$v"
}

# download VERSION ASSET -> $TMP/ASSET, verified against SHA256SUMS
download() {
    base="https://github.com/$REPO/releases/download/v$1"
    say "downloading $2"
    fetch "$base/SHA256SUMS" "$TMP/SHA256SUMS" || die "could not download SHA256SUMS"
    fetch "$base/$2" "$TMP/$2" || die "could not download $2"
    want=$(awk -v f="$2" '$2 == f || $2 == "*" f { print $1; exit }' "$TMP/SHA256SUMS")
    [ -n "$want" ] || die "$2 is not listed in SHA256SUMS"
    got=$(sha256 "$TMP/$2")
    [ "$want" = "$got" ] || die "checksum mismatch for $2 (expected $want, got $got)"
    say "checksum ok"
}

# Prints "deb", "rpm" or "" for the running distro.
package_kind() {
    [ -r /etc/os-release ] || return 0
    # shellcheck disable=SC1091
    ids=$(. /etc/os-release && printf '%s %s' "${ID:-}" "${ID_LIKE:-}")
    for id in $ids; do
        case "$id" in
            debian | ubuntu) echo deb; return 0 ;;
            fedora) echo rpm; return 0 ;;
        esac
    done
}

as_root() {
    if [ "$(id -u)" -eq 0 ]; then
        "$@"
    elif has sudo; then
        sudo "$@"
    else
        die "root is needed to install the package; run as root or use --user"
    fi
}

rc_files() {
    printf '%s\n' "$HOME/.profile" "$HOME/.bashrc" "$HOME/.zshrc"
}

add_path_lines() {
    rc_files | while IFS= read -r rc; do
        # ~/.profile always; the others only if that shell is set up.
        if [ "$rc" != "$HOME/.profile" ] && [ ! -e "$rc" ]; then
            continue
        fi
        if [ -f "$rc" ] && grep -qF "$MARK" "$rc"; then
            continue
        fi
        if [ -s "$rc" ] && [ -n "$(tail -c 1 "$rc")" ]; then
            printf '\n' >>"$rc"
        fi
        printf '%s %s\n' "$PATH_LINE" "$MARK" >>"$rc"
        say "added ~/.local/bin to PATH in $rc"
    done
}

remove_path_lines() {
    rc_files | while IFS= read -r rc; do
        if [ -f "$rc" ] && grep -qF "$MARK" "$rc"; then
            grep -vF "$MARK" "$rc" >"$TMP/rc" || true
            # Write back in place so the file keeps its mode, owner and links.
            cat "$TMP/rc" >"$rc"
            say "removed PATH line from $rc"
        fi
    done
}

hint_rehash() {
    say "if your shell still can't find nib, run 'hash -r' (bash/sh) or 'rehash' (zsh), or open a new terminal"
}

install_user() {
    v=$1
    name="nib-$v-x86_64-linux"
    download "$v" "$name.tar.gz"
    tar -xzf "$TMP/$name.tar.gz" -C "$TMP"
    [ -f "$TMP/$name/nib" ] || die "unexpected tarball layout"
    mkdir -p "$USER_BIN"
    # Copy beside the target, then rename: replaces a running nib safely.
    cp "$TMP/$name/nib" "$USER_BIN/.nib.new.$$"
    chmod 755 "$USER_BIN/.nib.new.$$"
    mv -f "$USER_BIN/.nib.new.$$" "$USER_BIN/nib"
    add_path_lines
    "$USER_BIN/nib" --version
    case ":$PATH:" in
        *":$USER_BIN:"*) ;;
        *) say "open a new terminal, or run '. ~/.profile', to put ~/.local/bin on PATH" ;;
    esac
    hint_rehash
}

install_deb() {
    v=$1
    name="nib_${v}_amd64.deb"
    download "$v" "$name"
    # Let apt's unprivileged downloader read the file.
    chmod 755 "$TMP"
    chmod 644 "$TMP/$name"
    as_root apt-get install -y "$TMP/$name"
    /usr/bin/nib --version
    hint_rehash
}

install_rpm() {
    v=$1
    name="nib-$v.x86_64.rpm"
    download "$v" "$name"
    as_root dnf install -y "$TMP/$name"
    /usr/bin/nib --version
    hint_rehash
}

uninstall() {
    purge=$1
    if [ -e "$USER_BIN/nib" ]; then
        rm -f "$USER_BIN/nib"
        say "removed $USER_BIN/nib"
    else
        say "no --user install found in $USER_BIN"
    fi
    remove_path_lines
    if [ "$purge" = 1 ]; then
        config="${XDG_CONFIG_HOME:-$HOME/.config}/nib"
        if [ -d "$config" ]; then
            rm -rf "$config"
            say "removed $config"
        fi
    fi
    data="${XDG_DATA_HOME:-$HOME/.local/share}/nib"
    if [ -d "$data" ]; then
        say "kept $data (it may hold unsaved changes from a crash); delete it by hand if you don't need it"
    fi
    # shellcheck disable=SC2016
    if has dpkg-query && dpkg-query -W -f '${Status}' nib 2>/dev/null | grep -q 'install ok installed'; then
        say "the system package is still installed; remove it with: sudo apt remove nib"
    elif has rpm && rpm -q nib >/dev/null 2>&1; then
        say "the system package is still installed; remove it with: sudo dnf remove nib"
    fi
    hint_rehash
}

main() {
    mode=auto
    action=install
    purge=0
    for arg in "$@"; do
        case "$arg" in
            --user) mode=user ;;
            --uninstall) action=uninstall ;;
            --purge) action=uninstall; purge=1 ;;
            -h | --help) usage; exit 0 ;;
            *) usage >&2; die "unknown option: $arg" ;;
        esac
    done

    TMP=$(mktemp -d) || die "could not create a temporary directory"
    trap cleanup EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM

    if [ "$action" = uninstall ]; then
        uninstall "$purge"
        return 0
    fi

    [ "$(uname -s)" = Linux ] || die "this installer is for Linux; on Windows use install.ps1"
    case "$(uname -m)" in
        x86_64 | amd64) ;;
        *) die "only x86_64 builds are published" ;;
    esac

    if [ "$mode" = auto ]; then
        kind=$(package_kind)
        case "$kind" in
            deb) has apt-get || kind="" ;;
            rpm) has dnf || kind="" ;;
        esac
        if [ -z "$kind" ]; then
            say "no apt (Debian/Ubuntu) or dnf (Fedora) found; installing into ~/.local/bin"
            mode=user
        else
            mode=$kind
        fi
    fi

    version=$(latest_version)
    say "installing nib $version"
    case "$mode" in
        user) install_user "$version" ;;
        deb) install_deb "$version" ;;
        rpm) install_rpm "$version" ;;
    esac
}

main "$@"
