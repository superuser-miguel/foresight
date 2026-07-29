#!/usr/bin/env bash
# Publish the signed OSTree repo that backs `flatpak update`.
#
#   scripts/publish-repo.sh            # build + sign + refresh docs/repo
#
# The repo is served straight off GitHub Pages out of docs/, so everything it
# writes is committed to git. Run this after tagging a release, then commit
# docs/ and push — Pages redeploys and clients see the new version.
#
# Requires: flatpak-builder, ostree, gpg with the release secret key.

set -euo pipefail
cd "$(dirname "$0")/.."

KEY="${FORESIGHT_GPG_KEY:-D67DB8E03D50A8C0}"
MANIFEST="io.github.superuser_miguel.Foresight.release.yml"
APP="io.github.superuser_miguel.Foresight"
BASE="https://superuser-miguel.github.io/foresight"

gpg --list-secret-keys "$KEY" >/dev/null 2>&1 \
    || { echo "error: no secret key $KEY — cannot sign the repo" >&2; exit 1; }

# Build from the pinned release tag, exporting straight into the Pages repo.
# Same manifest the bundle uses, so both artifacts come from identical bits.
flatpak-builder --user --force-clean \
    --repo=docs/repo --gpg-sign="$KEY" \
    build-dir-release "$MANIFEST"

# Debug symbols are ~2x the app and this repo lives in git forever. The
# published .flatpak bundle carries no debug either, so dropping it keeps the
# two distribution channels identical rather than subtly different.
ostree --repo=docs/repo refs --delete "runtime/$APP.Debug/x86_64/stable" 2>/dev/null || true

# Regenerates appstream + summary and signs both. Without a signed summary a
# client with GPGKey set refuses the remote outright.
flatpak build-update-repo --prune --prune-depth=20 \
    --title="Foresight" --default-branch=stable \
    --gpg-sign="$KEY" docs/repo

# The .flatpakref/.flatpakrepo embed the public key, so they must be rewritten
# whenever the signing key changes — not just when the app does.
PUB="$(gpg --export "$KEY" | base64 -w0)"

cat > docs/foresight.flatpakref <<EOF
[Flatpak Ref]
Title=Foresight
Name=$APP
Branch=stable
Url=$BASE/repo/
Homepage=$BASE/
Comment=See exactly what rsync will change — before a single byte moves
Description=A GNOME-native rsync front-end that previews every sync as a grouped change list.
Icon=$BASE/icon.svg
IsRuntime=false
RuntimeRepo=https://flathub.org/repo/flathub.flatpakrepo
SuggestRemoteName=foresight
GPGKey=$PUB
EOF

cat > docs/foresight.flatpakrepo <<EOF
[Flatpak Repo]
Title=Foresight
Url=$BASE/repo/
Homepage=$BASE/
Comment=Signed OSTree remote for Foresight releases
Description=The official Foresight repository. Adding it lets flatpak update pull new versions.
Icon=$BASE/icon.svg
DefaultBranch=stable
GPGKey=$PUB
EOF

# Pages runs Jekyll by default, which would swallow parts of the OSTree tree.
touch docs/.nojekyll
cp data/icons/hicolor/scalable/apps/$APP.svg docs/icon.svg

echo
echo "docs/repo refreshed ($(du -sh docs/repo | cut -f1)), signed with $KEY"
ostree --repo=docs/repo refs
echo
echo "Next: git add docs && git commit && git push   # Pages redeploys"
