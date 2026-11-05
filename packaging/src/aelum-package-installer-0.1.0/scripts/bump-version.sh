#!/usr/bin/env bash
#
# bump-version.sh — cut a new release in one command: bump the version where
# it lives, commit, push, build, publish to the [aelum] repo, and delete the
# superseded package from R2. Same shape as video-player/scripts/bump-version.sh.
#
# The version lives in three places, and Cargo.lock carries the crate version:
#   Cargo.toml               [package].version — the source of truth
#   packaging/PKGBUILD       pkgver (build-pkg.sh fails fast when they drift)
#   data/*.metainfo.xml      the AppStream release list users see
#   Cargo.lock               refreshed by `cargo check`; the PKGBUILD builds
#                            with `cargo build --release --locked`, so a stale
#                            lock fails the package build — refresh before the
#                            commit, not after.
#
# The old package is deleted from R2 only AFTER the publish verified the new
# DB entry: there is never a moment where the served DB names a missing file.
# (The edge caches the DB ~4 h — see repo/REPO.md.)
#
# Usage:
#   ./scripts/bump-version.sh 0.1.1                     # full release
#   ./scripts/bump-version.sh 0.1.1 --changes "Fixed X" [--changes "Y"]
#   ./scripts/bump-version.sh 0.1.1 --no-push          # rehearsal, no publish
#   ./scripts/bump-version.sh 0.1.1 --keep-old         # keep the old pkg on R2
#
# Flags:
#   --changes <text>   one AppStream changelog bullet (repeatable)
#   --no-push          commit locally only, skip push + publish + delete
#   --no-publish       build the package but skip the repo publish + delete
#   --keep-old         publish, but leave the superseded package on R2
#   --no-test          skip the test + clippy gate (rehearsals)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_DIR="$ROOT_DIR/../repo"
METAINFO="$ROOT_DIR/data/aelum.packageinstaller.Packages.metainfo.xml"

NEW_VER=""
NO_PUSH=false
NO_PUBLISH=false
KEEP_OLD=false
NO_TEST=false
CHANGES=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --changes)  CHANGES+=("$2"); shift 2 ;;
        --no-push)  NO_PUSH=true; shift ;;
        --no-publish) NO_PUBLISH=true; shift ;;
        --keep-old) KEEP_OLD=true; shift ;;
        --no-test)  NO_TEST=true; shift ;;
        *)
            if [[ -n "$NEW_VER" ]]; then echo "Usage: $0 <new-version> [flags]" >&2; exit 2; fi
            NEW_VER="$1"; shift ;;
    esac
done

if [[ ! "$NEW_VER" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Usage: $0 <new-version: X.Y.Z> [--changes <text>] [--no-push] [--no-publish] [--keep-old] [--no-test]" >&2
    exit 2
fi

cd "$ROOT_DIR"

# ── Preconditions ─────────────────────────────────────────────────────────────
# build-pkg.sh tars the *tracked tree*, and the commit is supposed to be
# exactly what was built and published — so tracked files must be clean first.
if [[ -n "$(git status --porcelain --untracked-files=no)" ]]; then
    echo "Error: uncommitted changes in tracked files — commit or stash them first." >&2
    exit 1
fi

CARGO_TOML="$ROOT_DIR/Cargo.toml"
PKGBUILD="$ROOT_DIR/packaging/PKGBUILD"

CUR_VER="$(awk '
    /^\[package\]/ { insec=1; next }
    /^\[/ { insec=0 }
    insec && /^version = / { gsub(/"/, ""); print $3; exit }
' "$CARGO_TOML")"
OLD_PKG_NAME="$(sed -n 's/^pkgname=\(.*\)$/\1/p' "$PKGBUILD" | head -n1)"
OLD_PKGREL="$(sed -n 's/^pkgrel=\(.*\)$/\1/p' "$PKGBUILD" | head -n1)"

if command -v vercmp >/dev/null 2>&1; then
    [[ "$(vercmp "$NEW_VER" "$CUR_VER")" -gt 0 ]] \
        || { echo "Error: $NEW_VER is not newer than $CUR_VER (vercmp)." >&2; exit 1; }
elif [[ "$NEW_VER" == "$CUR_VER" ]]; then
    echo "Error: $NEW_VER is the current version." >&2
    exit 1
fi

echo "==> Bumping $CUR_VER -> $NEW_VER"

# ── Version files ─────────────────────────────────────────────────────────────
# Only the version line INSIDE [package]: dependency tables have their own
# `version =` lines that must not move.
NEW_VER="$NEW_VER" awk '
    /^\[package\]/ { insec=1; print; next }
    /^\[/ { insec=0 }
    insec && /^version = / { sub(/"[^"]*"/, "\"" ENVIRON["NEW_VER"] "\""); insec=0 }
    { print }
' "$CARGO_TOML" > "$CARGO_TOML.new" && mv "$CARGO_TOML.new" "$CARGO_TOML"
grep -q "^version = \"$NEW_VER\"" "$CARGO_TOML" \
    || { echo "Error: Cargo.toml was not updated." >&2; exit 1; }

sed -i "s/^pkgver=.*/pkgver=$NEW_VER/" "$PKGBUILD"
grep -q "^pkgver=$NEW_VER$" "$PKGBUILD" \
    || { echo "Error: PKGBUILD was not updated." >&2; exit 1; }

# ── AppStream release entry (newest first, after the <releases> line) ────────
ITEMS=""
for c in "${CHANGES[@]+"${CHANGES[@]}"}"; do
    esc="$(printf '%s' "$c" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g')"
    ITEMS+="          <li>${esc}</li>"$'\n'
done
TODAY="$(date +%F)"
NEW_VER="$NEW_VER" TODAY="$TODAY" ITEMS="$ITEMS" awk '
    { print }
    /<releases>/ && !done {
        done = 1
        if (ENVIRON["ITEMS"] != "")
            printf "    <release version=\"%s\" date=\"%s\">\n      <description>\n        <ul>\n%s        </ul>\n      </description>\n    </release>\n",
                ENVIRON["NEW_VER"], ENVIRON["TODAY"], ENVIRON["ITEMS"]
        else
            printf "    <release version=\"%s\" date=\"%s\"/>\n", ENVIRON["NEW_VER"], ENVIRON["TODAY"]
    }
    END { if (!done) exit 1 }
' "$METAINFO" > "$METAINFO.new" && mv "$METAINFO.new" "$METAINFO"
grep -q "<release version=\"$NEW_VER\"" "$METAINFO" \
    || { echo "Error: the metainfo release entry was not added." >&2; exit 1; }

# ── Cargo.lock: makepkg builds with --locked, so the lock must match ─────────
echo "==> Refreshing Cargo.lock..."
cargo check >/dev/null

# ── Gate: the repo must stay green at a release ──────────────────────────────
if ! $NO_TEST; then
    echo "==> cargo test..."
    cargo test >/dev/null
    echo "==> cargo clippy --all-targets..."
    CLIPPY_LOG="$(mktemp)"
    cargo clippy --all-targets >"$CLIPPY_LOG" 2>&1 \
        || { echo "Error: clippy failed:" >&2; tail -20 "$CLIPPY_LOG" >&2; rm -f "$CLIPPY_LOG"; exit 1; }
    if grep -q "^warning" "$CLIPPY_LOG"; then
        echo "Error: clippy warnings — fix them before releasing." >&2
        grep "^warning" "$CLIPPY_LOG" | head -5 >&2
        rm -f "$CLIPPY_LOG"
        exit 1
    fi
    rm -f "$CLIPPY_LOG"
fi

# ── Commit + push ─────────────────────────────────────────────────────────────
git add "$CARGO_TOML" "$ROOT_DIR/Cargo.lock" "$PKGBUILD" "$METAINFO"
git commit -m "chore(release): $NEW_VER"
echo "==> Committed."
if ! $NO_PUSH; then
    git push
    echo "==> Pushed."
fi

# ── Build ─────────────────────────────────────────────────────────────────────
"$SCRIPT_DIR/build-pkg.sh"
# A rehearsal must not touch the repo: publishing a package whose release
# commit was never pushed would serve an artifact that no commit describes.
if $NO_PUSH || $NO_PUBLISH; then
    echo "==> Stopping after the build ($($NO_PUSH && echo '--no-push' || echo '--no-publish'))."
    $NO_PUSH && echo "    Rehearsal: undo the release commit with \`git reset --hard HEAD~1\`" \
               && echo "    and remove dist/*-$NEW_VER-*."
    exit 0
fi

# ── Publish (this repo's package, not the publisher's video-player default) ──
PKGFILE="$(find "$ROOT_DIR/dist" -maxdepth 1 -name "$OLD_PKG_NAME-[0-9]*.pkg.tar.zst" | sort | tail -n1)"
if [[ -z "$PKGFILE" ]]; then
    echo "Error: no built package found in dist/." >&2
    exit 1
fi
"$REPO_DIR/publish-repo.sh" "$PKGFILE"

# ── Delete the superseded package (after the new DB entry is verified) ───────
if $KEEP_OLD; then
    echo "==> --keep-old: leaving ${OLD_PKG_NAME}-${CUR_VER}-${OLD_PKGREL} on R2."
    exit 0
fi
if [[ ! -f "$REPO_DIR/release.env" ]]; then
    echo "Warning: $REPO_DIR/release.env not found — cannot delete the old package." >&2
    exit 0
fi
set -a; . "$REPO_DIR/release.env"; set +a
RCLONE_CONF="$(mktemp)"
cat > "$RCLONE_CONF" <<EOF
[r2]
type = s3
provider = Cloudflare
access_key_id = ${R2_ACCESS_KEY:?}
secret_access_key = ${R2_SECRET_KEY:?}
endpoint = https://${R2_ACCOUNT_ID:?}.r2.cloudflarestorage.com
no_check_bucket = true
acl = public-read
EOF
trap 'rm -f "$RCLONE_CONF"' EXIT
REPO_ARCH="${REPO_ARCH:-x86_64}"
OLD_BASE="${OLD_PKG_NAME}-${CUR_VER}-${OLD_PKGREL}-${REPO_ARCH}.pkg.tar.zst"
REMOTE="r2:${R2_BUCKET:-aelum}/repo/${REPO_ARCH}"
echo "==> Deleting the superseded package: $OLD_BASE (+ .sig)"
rclone --config "$RCLONE_CONF" delete "$REMOTE/$OLD_BASE"
rclone --config "$RCLONE_CONF" delete "$REMOTE/$OLD_BASE.sig"
if rclone --config "$RCLONE_CONF" ls "$REMOTE/" | grep -q "^.*${OLD_BASE}\$"; then
    echo "Error: $OLD_BASE is still on R2." >&2
    exit 1
fi
echo "    deleted OK"

echo
NEW_BASE="${OLD_PKG_NAME}-${NEW_VER}-${OLD_PKGREL}-${REPO_ARCH}.pkg.tar.zst"
echo "Released $NEW_VER:"
echo "  https://cdn.aelum.dev/repo/${REPO_ARCH}/${NEW_BASE}"
echo "Users: sudo pacman -Syu"
