#!/usr/bin/env bash
# Adds release <version> to a checkout of the gh-pages branch and commits it: the web files in
# v<version>/, with their SHA256SUMS, and the site at the root when <version> is the highest
# published. A published v*/ never changes: the script refuses a version already there, and any
# change under another version's directory.
#
# usage: pages.sh <version> <dist> <pages>
#   <version>  X.Y.Z, without the v
#   <dist>     web/dist after `npm run build`
#   <pages>    a clean checkout of gh-pages (an orphan branch for the first release)
set -euo pipefail

die() {
  echo "pages.sh: $*" >&2
  exit 1
}

[ $# = 3 ] || die "usage: pages.sh <version> <dist> <pages>"
version=$1
dist=$(cd "$2" && pwd)
pages=$3
[[ $version =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || die "$version is not X.Y.Z"
dir=v$version

# The files a page embeds from v<version>/. The Escapement Project's CI pins the first five by
# sha256.
files=(tickwave.js tickwave-worker.js tickwave.wasm tickwave-chart.js tickwave.css
  tickwave-convert-worker.js tickwave-convert.wasm)
for f in "${files[@]}"; do
  [ -f "$dist/$f" ] || die "$dist/$f is missing: run npm run build"
done
[ -f "$dist/site/index.html" ] || die "$dist/site/index.html is missing: run npm run build"

cd "$pages"
[ -z "$(git status --porcelain)" ] || die "$pages has uncommitted changes"
[ ! -e "$dir" ] || die "$dir/ is published already, and a published version never changes"

mkdir "$dir"
for f in "${files[@]}"; do
  cp "$dist/$f" "$dir/"
  [ ! -f "$dist/$f.map" ] || cp "$dist/$f.map" "$dir/"
done
(cd "$dir" && sha256sum -- * > SHA256SUMS)

# A version's directory, and nothing else at the root, is named vX.Y.Z.
release='^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'
shopt -s dotglob nullglob
published=()
for e in *; do
  if [ -d "$e" ] && [[ $e =~ $release ]]; then published+=("${e#v}"); fi
done

# The root is the site of the highest version published, this one included.
highest=$(printf '%s\n' "${published[@]}" | sort -V | tail -1)
if [ "$highest" = "$version" ]; then
  for e in *; do
    if [ "$e" = .git ] || { [ -d "$e" ] && [[ $e =~ $release ]]; }; then continue; fi
    rm -rf -- "$e"
  done
  cp -R "$dist/site/." .
  touch .nojekyll
fi
shopt -u dotglob nullglob

git add -A
# Every change under a version's directory is a file added to this one.
bad=$(git diff --cached --name-status --no-renames | awk -F '\t' -v d="$dir/" '
  $2 ~ /^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\// && !($1 == "A" && index($2, d) == 1)')
if [ -n "$bad" ]; then
  git reset -q --hard
  git clean -qfd
  die "refused: this would change a published version's files:
$bad"
fi
if [ "$highest" != "$version" ]; then
  outside=$(git diff --cached --name-only | grep -v "^$dir/" || true)
  [ -z "$outside" ] || die "refused: v$highest is the latest, so the site stays as it is:
$outside"
fi
git commit -q -m "tickwave v$version"
echo "pages.sh: committed v$version$([ "$highest" = "$version" ] && echo ', and the site at the root')"
