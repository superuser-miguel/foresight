#!/usr/bin/env bash
# Publish Foresight to its signed, auto-updating Flatpak repo.
#
#   scripts/publish-repo.sh            # build + sign + force-push foresight-repo
#
# Foresight ships two ways: a one-off .flatpak bundle on GitHub Releases, and
# this hosted OSTree repo at https://superuser-miguel.github.io/foresight-repo/
# that `flatpak update` tracks.
#
# Layout choice (deliberate, shared with septima-repo / Vivid_Gradience-repo /
# alacritty-flatpak-repo): the published repo is regenerated wholesale and
# **force-pushed as a single commit** each release, so its git history never
# accumulates superseded, content-addressed OSTree objects. It is a separate
# GitHub repo from the code — the code repo stays clean. Foresight served this
# out of its own docs/ until v0.1.2; that welded every published binary into the
# source history permanently, which is exactly what this layout avoids.
#
# Prerequisites:
#   - flatpak-builder, ostree, git, gpg
#   - the signing secret key present in the local GPG keyring (see KEY below);
#     losing it means you can no longer publish trusted updates to this remote.
#   - push access to git@github.com:superuser-miguel/foresight-repo.git

set -euo pipefail
cd "$(dirname "$0")/.."

KEY="${FORESIGHT_GPG_KEY:-D67DB8E03D50A8C0}"   # signs the OSTree repo; public key is baked into the .flatpakref
MANIFEST="io.github.superuser_miguel.Foresight.release.yml"
APP="io.github.superuser_miguel.Foresight"
PAGES_URL="https://superuser-miguel.github.io/foresight-repo"
SITE_URL="https://superuser-miguel.github.io/foresight"
PUBLISH_REMOTE="git@github.com:superuser-miguel/foresight-repo.git"

gpg --list-secret-keys "$KEY" >/dev/null 2>&1 \
    || { echo "error: no secret key $KEY — cannot sign the repo" >&2; exit 1; }

# Must live on the same filesystem as the flatpak-builder state dir, so NOT in
# /tmp — that is tmpfs here, which flatpak-builder rejects outright ("state dir
# is not on the same filesystem as the target dir") and which would put the
# whole cargo build in RAM anyway. Kept inside the project and gitignored.
here="$PWD"
work="$(mktemp -d "$here/.publish-tmp.XXXXXX")"
trap 'rm -rf "$work"' EXIT
repo="$work/repo"

echo ">> Building signed release into a fresh OSTree repo…"
# Built from the pinned release tag, same manifest the bundle uses, so both
# distribution channels come from identical bits.
flatpak-builder --user --force-clean --state-dir="$work/state" \
    --repo="$repo" --gpg-sign="$KEY" "$work/build-dir" "$MANIFEST"

# Debug symbols are ~2x the app. The published .flatpak bundle carries no debug
# either, so dropping it keeps the two distribution channels identical rather
# than subtly different.
ostree --repo="$repo" refs --delete "runtime/$APP.Debug/x86_64/stable" 2>/dev/null || true

# Regenerates appstream + summary and signs both. Without a signed summary a
# client with GPGKey set refuses the remote outright.
echo ">> Generating static deltas + signing the summary…"
flatpak build-update-repo --generate-static-deltas --prune \
    --title="Foresight" --default-branch=stable \
    --gpg-sign="$KEY" "$repo"

echo ">> Assembling the publish tree (repo + .flatpakref + landing page)…"
pub="$work/publish"
mkdir -p "$pub"
cp -a "$repo" "$pub/repo"
touch "$pub/.nojekyll"   # serve OSTree byte-for-byte; do not let Jekyll rewrite it
cp "data/icons/hicolor/scalable/apps/$APP.svg" "$pub/icon.svg"

# The .flatpakref/.flatpakrepo embed the public key, so they must be rewritten
# whenever the signing key changes — not just when the app does.
PUB="$(gpg --export "$KEY" | base64 -w0)"

cat > "$pub/foresight.flatpakref" <<EOF
[Flatpak Ref]
Title=Foresight
Name=$APP
Branch=stable
Url=$PAGES_URL/repo/
Homepage=$SITE_URL/
Comment=See exactly what rsync will change — before a single byte moves
Description=A GNOME-native rsync front-end that previews every sync as a grouped change list.
Icon=$PAGES_URL/icon.svg
IsRuntime=false
RuntimeRepo=https://flathub.org/repo/flathub.flatpakrepo
SuggestRemoteName=foresight
GPGKey=$PUB
EOF

cat > "$pub/foresight.flatpakrepo" <<EOF
[Flatpak Repo]
Title=Foresight
Url=$PAGES_URL/repo/
Homepage=$SITE_URL/
Comment=Signed OSTree remote for Foresight releases
Description=The official Foresight repository. Adding it lets flatpak update pull new versions.
Icon=$PAGES_URL/icon.svg
DefaultBranch=stable
GPGKey=$PUB
EOF

cat > "$pub/index.html" <<'EOF'
<!doctype html><meta charset=utf-8><title>Foresight — Flatpak repo</title>
<style>body{font-family:system-ui,sans-serif;max-width:40rem;margin:4rem auto;padding:0 1rem;line-height:1.6}code{background:#f0f0f0;padding:.1em .3em;border-radius:3px}</style>
<h1>Foresight — signed Flatpak repo</h1>
<p>Automatic updates for <a href="https://superuser-miguel.github.io/foresight/">Foresight</a>, the rsync front-end that previews every change before a byte moves.</p>
<pre><code>flatpak install --user https://superuser-miguel.github.io/foresight-repo/foresight.flatpakref
flatpak run io.github.superuser_miguel.Foresight</code></pre>
<p>Updates then arrive with <code>flatpak update</code>. Signed with the project's GPG key.</p>
EOF

echo ">> Force-pushing as a single squashed commit…"
version="$(date +%Y-%m-%d)"
git -C "$pub" init -q -b main
git -C "$pub" add -A
git -C "$pub" -c user.name=superuser-miguel \
    -c user.email=16271056+superuser-miguel@users.noreply.github.com \
    commit -q -m "Publish Foresight (${version}) — signed OSTree repo + .flatpakref"
git -C "$pub" remote add origin "$PUBLISH_REMOTE"
git -C "$pub" push -u --force origin main

echo
echo ">> Done. Verify from the public URL:"
echo "   flatpak install --user ${PAGES_URL}/foresight.flatpakref"
