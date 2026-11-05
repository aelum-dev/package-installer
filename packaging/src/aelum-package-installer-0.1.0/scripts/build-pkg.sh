#!/usr/bin/env bash
#
# build-pkg.sh — build + sign an Arch package (.pkg.tar.zst) of the package
# manager. Same shape as video-player/scripts/build-pkg.sh:
#
#   1. Checks packaging/PKGBUILD's pkgver against Cargo.toml's [package]
#      version (Cargo.toml is the source of truth; a drift fails fast).
#   2. Tars the tracked tree (`git ls-files`) into packaging/ as the PKGBUILD's
#      source — a clean checkout, no target/ and no untracked scratch files.
#   3. Runs makepkg.
#   4. Moves the package to dist/, writes a SHA-256 file, and produces a
#      detached ASCII-armored GPG signature (AELUM_GPG_KEY; optional
#      AELUM_GPG_PRIVATE_KEY_B64 + GPG_PASSPHRASE for headless signing).
#      --no-sign skips signing for a quick install test.
#   5. Verifies the checksum and signature before returning.
#
# Publish to the [aelum] repository with:
#   ../repo/publish-repo.sh package-installer/dist/<the .pkg.tar.zst>
#
# Usage:
#   ./scripts/build-pkg.sh              # build + sign (needs AELUM_GPG_KEY)
#   ./scripts/build-pkg.sh --no-sign    # build only, for install testing
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
PKG_DIR="$ROOT_DIR/packaging"
DIST_DIR="$ROOT_DIR/dist"

# Local secrets (gitignored) if present, so signing works without exporting
# the vars by hand — same shape as the distro's release.env.
if [ -f "$ROOT_DIR/release.env" ]; then
    set -a; . "$ROOT_DIR/release.env"; set +a
fi

NO_SIGN=false
for arg in "$@"; do
    case "$arg" in
        --no-sign) NO_SIGN=true ;;
        *) echo "Usage: $0 [--no-sign]" >&2; exit 2 ;;
    esac
done

command -v makepkg >/dev/null 2>&1 || { echo "Error: makepkg not found (Arch host expected)." >&2; exit 1; }
command -v git >/dev/null 2>&1 || { echo "Error: git not found." >&2; exit 1; }

# ── Version: Cargo.toml is the source of truth, the PKGBUILD must agree ─────
CARGO_VER="$(awk -F'"' '/^\[package\]/ { pkg=1; next } /^\[/ && !/^\[package\]/ { pkg=0 } pkg && /^version = / { print $2; exit }' "$ROOT_DIR/Cargo.toml")"
PKG_NAME="$(sed -n 's/^pkgname=\(.*\)$/\1/p' "$PKG_DIR/PKGBUILD" | head -n1)"
PKG_VER="$(sed -n 's/^pkgver=\(.*\)$/\1/p' "$PKG_DIR/PKGBUILD" | head -n1)"
if [[ -z "$CARGO_VER" || -z "$PKG_NAME" || -z "$PKG_VER" ]]; then
    echo "Error: could not read the version from Cargo.toml / the PKGBUILD." >&2
    exit 1
fi
if [[ "$CARGO_VER" != "$PKG_VER" ]]; then
    echo "Error: PKGBUILD pkgver ($PKG_VER) != Cargo.toml version ($CARGO_VER)." >&2
    echo "Bump packaging/PKGBUILD to match [package].version." >&2
    exit 1
fi

# ── Source tarball from the tracked tree ─────────────────────────────────────
TARBALL="$PKG_DIR/$PKG_NAME-$PKG_VER.tar.gz"
echo "==> Source tarball: $TARBALL"
(cd "$ROOT_DIR" && git ls-files -z | tar --null -T - -czf "$TARBALL" \
    --transform "s,^,$PKG_NAME-$PKG_VER/,")

# ── makepkg ──────────────────────────────────────────────────────────────────
echo "==> Building $PKG_NAME-$PKG_VER with makepkg..."
(cd "$PKG_DIR" && makepkg -f)

# The main package: the name is followed by the version (a digit), which
# keeps any stray subpackage out of the pick.
PKGFILE="$(find "$PKG_DIR" -maxdepth 1 -name "$PKG_NAME-[0-9]*.pkg.tar.*" | sort | tail -n1)"
if [[ -z "$PKGFILE" ]]; then
    echo "Error: makepkg produced no package file." >&2
    exit 1
fi
mkdir -p "$DIST_DIR"
mv "$PKGFILE" "$DIST_DIR/"
PKGFILE="$DIST_DIR/$(basename "$PKGFILE")"
BASE="$(basename "$PKGFILE")"
echo "==> Package: $PKGFILE"

# ── Checksum (basename-only, so `sha256sum -c` works next to the download) ──
( cd "$DIST_DIR" && sha256sum "$BASE" > "$BASE.sha256" )
echo "==> Checksum: $DIST_DIR/$BASE.sha256"

# ── GPG detached signature ───────────────────────────────────────────────────
if ! $NO_SIGN; then
    command -v gpg >/dev/null 2>&1 || { echo "Error: gpg not found (or use --no-sign)." >&2; exit 1; }
    if [[ -z "${AELUM_GPG_KEY:-}" ]]; then
        echo "Error: set AELUM_GPG_KEY (key id or 'Name <email>'), or use --no-sign." >&2
        echo "  List keys:  gpg --list-secret-keys --keyid-format=long" >&2
        exit 1
    fi

    # Optional headless signing (same as the distro's sign-iso.sh): import the
    # private key from the environment and allow loopback pinentry.
    if [[ -n "${AELUM_GPG_PRIVATE_KEY:-}" || -n "${AELUM_GPG_PRIVATE_KEY_B64:-}" ]]; then
        KEY_ARMOR="${AELUM_GPG_PRIVATE_KEY:-}"
        if [[ -n "${AELUM_GPG_PRIVATE_KEY_B64:-}" ]]; then
            KEY_ARMOR="$(printf '%s' "$AELUM_GPG_PRIVATE_KEY_B64" | base64 -d)"
        fi
        echo "==> Importing signing key..."
        printf '%s' "$KEY_ARMOR" | gpg --import --batch 2>&1 | tail -2
        printf 'trust\n5\ny\n' | gpg --command-fd 0 --batch --yes --edit-key "$AELUM_GPG_KEY" >/dev/null 2>&1 || true
    fi
    mkdir -p "${GNUPGHOME:-$HOME/.gnupg}"
    AGENT_CONF="${GNUPGHOME:-$HOME/.gnupg}/gpg-agent.conf"
    if ! grep -q 'allow-loopback-pinentry' "$AGENT_CONF" 2>/dev/null; then
        echo 'allow-loopback-pinentry' >> "$AGENT_CONF"
        # Only restart the agent when its config actually changed. Killing it
        # every run races the dying agent against the next gpg call, which
        # then falls back to a /dev/tty prompt (no tty in a script).
        gpgconf --kill gpg-agent 2>/dev/null || true
    fi

    GPG_EXTRA=()
    if [[ -n "${GPG_PASSPHRASE:-}" ]]; then
        GPG_EXTRA=(--pinentry-mode loopback --passphrase "$GPG_PASSPHRASE")
    fi
    SIG="$PKGFILE.asc"
    # --yes: the output is a generated artifact; overwrite any stale signature.
    gpg --batch --yes --local-user "$AELUM_GPG_KEY" \
        --armor --detach-sign --digest-algo=SHA512 \
        "${GPG_EXTRA[@]}" \
        --output "$SIG" "$PKGFILE"
    echo "==> Signature: $SIG"
fi

# ── Verify what we just produced ─────────────────────────────────────────────
echo "==> Verifying..."
( cd "$DIST_DIR" && sha256sum -c "$BASE.sha256" )
if ! $NO_SIGN; then
    gpg --verify "$PKGFILE.asc" "$PKGFILE" && echo "==> GPG signature: OK"
fi

cat <<EOF

Publish to the [aelum] repository:

    ../repo/publish-repo.sh $PKGFILE

Or install directly from disk on a test host:

    sudo pacman -U $PKGFILE

EOF
