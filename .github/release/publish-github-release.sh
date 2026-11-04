#!/bin/sh
# Mirrors a published release's downloads to its GitHub release, byte for byte,
# as Clip.dmg (to install) and Clip.zip (what the updater installs). A stable
# X.Y.Z release is GitHub's "Latest", so
# https://github.com/anomalyco/clip/releases/latest/download/Clip.dmg is always
# the newest stable DMG; a dev build (0.0.0-dev-<run>) is a prerelease and
# never Latest. Release runs it last, after tag-release.sh tagged the release
# commit; it never creates the tag itself.
#
#   publish-github-release.sh <dist>
#
# Environment: CLIP_VERSION, CLIP_RELEASE_COMMIT, CLIP_RELEASE_TAG,
# CLIP_RELEASE_SOURCE (a checkout of the commit with docs/releases/), GH_TOKEN.
set -eu

repo=${CLIP_RELEASE_REPO:-anomalyco/clip}
[ $# -eq 1 ] || {
  echo "Usage: $0 <dist>" >&2
  exit 1
}
dist=$1
version=${CLIP_VERSION:?Set CLIP_VERSION}
commit=${CLIP_RELEASE_COMMIT:?Set CLIP_RELEASE_COMMIT}
tag=${CLIP_RELEASE_TAG:?Set CLIP_RELEASE_TAG}
source=${CLIP_RELEASE_SOURCE:?Set CLIP_RELEASE_SOURCE}
[ "$tag" = "v$version" ] || {
  echo "The release tag is $tag, not v$version." >&2
  exit 1
}

temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

if printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
  title="Clip $version"
  notes="$source/docs/releases/$version.md"
  test -f "$notes" || {
    echo "Add release notes at docs/releases/$version.md." >&2
    exit 1
  }
  set -- --latest=true
elif printf '%s\n' "$version" | grep -Eq '^0\.0\.0-dev-[0-9]+$'; then
  title="Clip dev ${version#0.0.0-dev-}"
  notes="$temporary/notes.md"
  printf 'A dev build of [%s](https://github.com/%s/commit/%s). It updates itself to each new dev build.\n\nFor the current release, download [Clip.dmg from the latest release](https://github.com/%s/releases/latest/download/Clip.dmg).\n' \
    "$(printf '%s' "$commit" | cut -c1-7)" "$repo" "$commit" "$repo" > "$notes"
  set -- --prerelease --latest=false
else
  echo "Only X.Y.Z and 0.0.0-dev-<run> releases go to GitHub, not $version." >&2
  exit 1
fi

# The published names carry no version, so a link to the latest release's file never changes.
mkdir "$temporary/assets" "$temporary/mirror"
cp "$dist/clip-$version-macos-arm64.dmg" "$temporary/assets/Clip.dmg"
cp "$dist/clip-$version-macos-arm64.zip" "$temporary/assets/Clip.zip"

tagged=$(gh api "repos/$repo/git/ref/tags/$tag" --jq .object.sha)
if [ "$tagged" != "$commit" ]; then
  echo "Tag $tag names $tagged, not $commit." >&2
  exit 1
fi
if ! gh release view "$tag" -R "$repo" --json name >/dev/null 2>&1; then
  gh release create "$tag" -R "$repo" --verify-tag --title "$title" --notes-file "$notes" "$@"
fi
gh release view "$tag" -R "$repo" --json assets > "$temporary/release.json"
for name in Clip.dmg Clip.zip; do
  if ! jq -e --arg name "$name" '.assets | any(.name == $name)' "$temporary/release.json" >/dev/null; then
    gh release upload "$tag" "$temporary/assets/$name" -R "$repo"
  fi
  gh release download "$tag" -R "$repo" --pattern "$name" --dir "$temporary/mirror" --clobber
  cmp -s "$temporary/assets/$name" "$temporary/mirror/$name" || {
    echo "GitHub's $name differs from the release bytes." >&2
    exit 1
  }
done
echo "https://github.com/$repo/releases/tag/$tag"
