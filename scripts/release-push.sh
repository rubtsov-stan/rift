#!/usr/bin/env bash
# Tags a release commit and pushes the tag with release/vX.Y.Z at the same
# commit in one atomic push. See RELEASING.md step 3.
#
#   scripts/release-push.sh vX.Y.Z [commit]    # commit defaults to origin/main
set -euo pipefail

tag=${1:?usage: scripts/release-push.sh vX.Y.Z [commit]}
[[ $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "release-push: not a version tag: $tag" >&2; exit 2; }

git fetch -q origin
commit=$(git rev-parse --verify "${2:-origin/main}^{commit}")
version=$(git show "$commit:Cargo.toml" | sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p')
[ "$version" = "${tag#v}" ] || { echo "release-push: Cargo.toml at $commit says $version, not ${tag#v}" >&2; exit 1; }

git tag -a "$tag" -m "$tag" "$commit"
git push --atomic origin "$commit:refs/heads/release/$tag" "refs/tags/$tag"
