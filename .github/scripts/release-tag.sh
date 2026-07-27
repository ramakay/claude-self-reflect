#!/usr/bin/env bash
#
# Keep a release tag and the artifacts published under it describing the same
# commit.
#
#   release-tag.sh claim   — take ownership of TAG at GITHUB_SHA, or prove an
#                            existing TAG already names GITHUB_SHA.
#   release-tag.sh verify  — re-prove that, with no side effects. Run again
#                            immediately before anything becomes public, since
#                            a ref is mutable and the claim may be stale.
#
# Every failure path refuses to publish. A lookup that did not clearly succeed
# is never read as "the tag is absent".
#
# Limit worth stating plainly: refs are mutable, and verification and whatever
# acts on the result are always separate API calls. This narrows the window in
# which a tag can be swapped under a release; it cannot close it. Only a
# repository ruleset forbidding update and deletion of release tags does that.
#
# Environment: GH_TOKEN, GITHUB_REPOSITORY, GITHUB_SHA, TAG

set -euo pipefail

MODE="${1:-}"
case "$MODE" in
  claim|verify) ;;
  *) echo "::error::Usage: release-tag.sh claim|verify" >&2; exit 2 ;;
esac

: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
: "${GITHUB_SHA:?GITHUB_SHA is required}"
: "${TAG:?TAG is required}"

command -v gh >/dev/null 2>&1 || { echo "::error::gh CLI unavailable on this runner."; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "::error::jq unavailable on this runner."; exit 1; }

# TAG is interpolated into API URL paths. git permits characters there that are
# URL-reserved — "v9#rc" is a legal ref name, but as a URL it truncates at the
# fragment and would address the tag "v9" instead. Rather than escape and hope,
# accept only the release shape this project actually uses.
if ! printf '%s' "$TAG" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z]+(\.[0-9A-Za-z]+)*)?$'; then
  echo "::error::Refusing to release with tag '${TAG}' — expected vMAJOR.MINOR.PATCH[-prerelease]."
  exit 1
fi

# Resolve TAG to the commit it names.
#   prints the commit sha and returns 0  — tag exists
#   returns 3                            — tag confirmed absent (HTTP 404)
#   returns 1                            — anything else: refuse to publish
resolve_tag_commit() {
  local resp http_status body object_sha object_type commit
  set +e
  resp="$(gh api -i "repos/${GITHUB_REPOSITORY}/git/ref/tags/${TAG}" 2>/dev/null)"
  set -e
  if [ -z "$resp" ]; then
    echo "::error::Tag lookup for ${TAG} returned no response." >&2
    return 1
  fi

  # gh api -i emits the status line, so a confirmed 404 can be told apart from
  # 401/403/429/5xx. Not named "status": read-only in some shells.
  http_status="$(printf '%s\n' "$resp" | head -n1 | tr -d '\r' | awk '{print $2}')"
  # Header/body split is the first empty line.
  body="$(printf '%s\n' "$resp" | awk 'b{print} /^\r?$/{b=1}')"

  case "$http_status" in
    200) ;;
    404) return 3 ;;
    *)
      echo "::error::Tag lookup for ${TAG} returned HTTP ${http_status:-<none>}. Refusing to publish." >&2
      return 1
      ;;
  esac

  object_sha="$(printf '%s' "$body" | jq -r '.object.sha // empty')"
  object_type="$(printf '%s' "$body" | jq -r '.object.type // empty')"
  if [ -z "$object_sha" ] || [ -z "$object_type" ]; then
    echo "::error::Unexpected ref response shape for tag ${TAG}. Refusing to publish." >&2
    return 1
  fi

  # Annotated tags point at a tag object; peel it to reach the commit.
  if [ "$object_type" = "tag" ]; then
    commit="$(gh api "repos/${GITHUB_REPOSITORY}/git/tags/${object_sha}" --jq '.object.sha // empty' 2>/dev/null || true)"
  else
    commit="$object_sha"
  fi
  if [ -z "$commit" ]; then
    echo "::error::Could not resolve tag ${TAG} to a commit." >&2
    return 1
  fi

  printf '%s' "$commit"
}

require_tag_names_built_commit() {
  local commit rc
  set +e
  commit="$(resolve_tag_commit)"
  rc=$?
  set -e
  case "$rc" in
    0) ;;
    3) echo "::error::Tag ${TAG} does not exist at a point where it must. Refusing to publish."; exit 1 ;;
    *) exit 1 ;;
  esac
  if [ "$commit" != "$GITHUB_SHA" ]; then
    echo "::error::Tag ${TAG} names ${commit}, but these artifacts were built from ${GITHUB_SHA}. Refusing to publish."
    exit 1
  fi
  echo "Tag ${TAG} names the built commit ${GITHUB_SHA}."
}

if [ "$MODE" = "verify" ]; then
  require_tag_names_built_commit
  exit 0
fi

# claim: ref creation is rejected when the ref already exists, so the API — not
# timing — decides which concurrent run owns an absent tag.
if gh api --method POST "repos/${GITHUB_REPOSITORY}/git/refs" \
     -f "ref=refs/tags/${TAG}" -f "sha=${GITHUB_SHA}" >/dev/null 2>&1; then
  echo "Created tag ${TAG} at the built commit ${GITHUB_SHA}."
  exit 0
fi

# The claim failed. Expected when the tag already exists (tag-push runs always
# land here), but identical in shape to an auth or transport failure — so let
# resolve_tag_commit branch on the actual status rather than assuming.
require_tag_names_built_commit
