#!/bin/sh
# Publishes Clip to the Anomaly release service through its shared client,
# vendored as release-client.sh (anomalyco/release docs/publishing.md), so
# nothing fetched at run time executes here.
# The client trades the job's GitHub OIDC token for a one-hour publish
# session, uploads (skipping files already stored), retries with backoff and
# checks the published files. Runs only in the Release workflow's publish
# job, which has `id-token: write`.
#
#   release-service.sh publish <dist> <notes-url>   # upload every target, then the release
#
# RELEASE is https://anoma.ly/release; CLIP_VERSION the version;
# CLIP_RELEASE_CHANNEL the channel (default stable). Writes <dist>/manifest.json.
# A rerun of the channel's current version succeeds when it is exactly these files.
set -eu

: "${RELEASE:?Set RELEASE to the release service origin}"
: "${CLIP_VERSION:?Set CLIP_VERSION}"
channel=${CLIP_RELEASE_CHANNEL:-stable}
case "$RELEASE" in
  https://anoma.ly/release) ;;
  *) echo "RELEASE must be https://anoma.ly/release." >&2; exit 1 ;;
esac

temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
client() { RELEASE=$RELEASE bash "$(dirname "$0")/release-client.sh" "$@"; }

publish() {
  dist=$1
  notes_url=$2
  version=$CLIP_VERSION
  # target=local file=published name. Published names carry no version or
  # target: the sha256 in the path tells files apart.
  set --
  for entry in \
    "macos-arm64=clip-$version-macos-arm64.zip=Clip.zip" \
    "macos-arm64-dmg=clip-$version-macos-arm64.dmg=Clip.dmg"
  do
    target=${entry%%=*}
    rest=${entry#*=}
    local_name=${rest%%=*}
    name=${rest#*=}
    [ -f "$dist/$local_name" ] || { echo "Missing $local_name in $dist." >&2; exit 1; }
    set -- "$@" --target "$target=$dist/$local_name=$name"
  done
  jq -n --arg channel "$channel" --arg version "$version" --arg notes_url "$notes_url" \
    '{channel: $channel, version: $version, notes_url: $notes_url, distributions: [{kind: "direct"}]}' > "$temporary/release.json"
  client publish --app clip --artifact desktop --release "$temporary/release.json" \
    --out "$dist/manifest.json" --verify-downloads "$@"
}

case "${1:-}" in
  publish)
    [ $# -eq 3 ] || { echo "Usage: $0 publish <dist> <notes-url>" >&2; exit 1; }
    publish "$2" "$3"
    ;;
  *)
    echo "Usage: $0 publish <dist> <notes-url>" >&2
    exit 1
    ;;
esac
