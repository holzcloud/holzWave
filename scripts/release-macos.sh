#!/bin/sh
# Build the macOS app locally, pack it as a DMG, and optionally publish it as a GitHub release and bump the Homebrew
# cask. Everything runs on this machine; there is no CI (owner rule).
#
#   sh scripts/release-macos.sh <version>                       build and pack only, print the cask fields
#   sh scripts/release-macos.sh <version> --publish             also create the GitHub release
#   sh scripts/release-macos.sh <version> --publish --tap <dir> also write the cask into a checkout of the tap
#
# <version> must equal the version in app/src-tauri/tauri.conf.json (the tag is v<version>). A version with a
# hyphen (0.0.1-alpha1, 0.0.1-beta1) is a GitHub prerelease and goes to the preview cask `holzwave@preview`; a
# version without one is a stable release and goes to `holzwave` and also to the preview cask, so the preview
# channel never lags behind stable.
#
# The app is signed ad hoc only (no Developer ID, no notarisation: Phase 10). The cask removes the quarantine
# attribute after the install and says so in its caveats.
#
# Needs a clean checkout of main at the commit to release, the gh CLI logged in for --publish, and the build
# environment of docs/building.md (scripts/env.sh puts Rust and Zig 0.16 on the PATH).

here=$(cd "$(dirname "$0")" && pwd)
cd "$here/.." || exit 2
. "$here/env.sh"

version=${1:-}
[ -n "$version" ] || { echo "usage: sh scripts/release-macos.sh <version> [--publish] [--tap <dir>]" >&2; exit 2; }
shift
publish=0
tap=
while [ $# -gt 0 ]; do
  case "$1" in
    --publish) publish=1 ;;
    --tap) shift; tap=${1:-}; [ -d "$tap" ] || { echo "--tap needs a directory" >&2; exit 2; } ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

conf=$(sed -n 's/^ *"version": "\(.*\)",/\1/p' app/src-tauri/tauri.conf.json | head -1)
[ "$conf" = "$version" ] || { echo "version $version is not the version of tauri.conf.json ($conf)" >&2; exit 1; }
[ "$(git rev-parse --abbrev-ref HEAD)" = main ] || { echo "release from main only" >&2; exit 1; }
[ -z "$(git status --porcelain)" ] || { echo "the working tree is not clean" >&2; exit 1; }
git fetch -q origin main && [ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] \
  || { echo "main is not the same commit as origin/main: push or pull first" >&2; exit 1; }

arch=$(uname -m)
[ "$arch" = arm64 ] || { echo "this script builds the Apple Silicon app; this machine is $arch" >&2; exit 1; }
name="holzWave_${version}_aarch64"
out=target/release/bundle
tag="v$version"

npm --prefix app ci >/dev/null && npm --prefix app run build >/dev/null || exit 1
npm --prefix app run tauri -- build --bundles app || exit 1
git checkout -- app/src-tauri/Cargo.toml 2>/dev/null # the Tauri CLI rewrites feature lists there; the commit stays as it is

# The Tauri DMG step scripts the Finder and times out without the Automation permission, so the DMG is made directly.
stage=$(mktemp -d) || exit 1
cp -R "$out/macos/holzWave.app" "$stage/" && ln -s /Applications "$stage/Applications" || exit 1
mkdir -p "$out/release"
dmg="$out/release/$name.dmg"
rm -f "$dmg"
hdiutil create -volname holzWave -srcfolder "$stage" -ov -format UDZO "$dmg" >/dev/null || exit 1
rm -rf "$stage"
sum=$(shasum -a 256 "$dmg" | cut -d' ' -f1)
echo "$sum  $name.dmg" > "$out/release/$name.dmg.sha256"
echo "built $dmg"
echo "sha256 $sum"

if [ "$publish" = 1 ]; then
  git ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null 2>&1 && { echo "tag $tag exists already" >&2; exit 1; }
  flag=
  case "$version" in *-*) flag=--prerelease ;; esac
  notes=$(mktemp)
  {
    echo "holzWave $version for macOS (Apple Silicon)."
    echo
    echo "- Unsigned: the app is signed ad hoc only. Install with Homebrew (the cask removes the quarantine attribute):"
    echo "  \`brew install --cask holzcloud/tap/holzwave@preview\`"
    echo "- Or open the DMG, drag holzWave to Applications, then right-click and Open once."
    echo "- SHA-256: \`$sum\`"
    echo "- Windows and Linux are not part of this release."
  } > "$notes"
  gh release create "$tag" "$dmg" "$out/release/$name.dmg.sha256" --target "$(git rev-parse HEAD)" \
    --title "holzWave $version" --notes-file "$notes" $flag || exit 1
  rm -f "$notes"
fi

if [ -n "$tap" ]; then
  write_cask() { # <token> <file>
    cat > "$2" <<CASK
cask "$1" do
  version "$version"
  sha256 "$sum"

  url "https://github.com/holzcloud/holzWave/releases/download/v#{version}/holzWave_#{version}_aarch64.dmg"
  name "holzWave"
  desc "Terminal emulator with tiled panes, tabs and workspaces"
  homepage "https://holzcloud.ch/holzwave"

  depends_on arch: :arm64
  depends_on macos: ">= :ventura"

  app "holzWave.app"

  # The app is signed ad hoc only: without this Gatekeeper blocks the first start.
  postflight do
    system_command "/usr/bin/xattr",
                   args: ["-cr", "#{appdir}/holzWave.app"]
  end

  zap trash: [
    "~/Library/Application Support/ch.holzcloud.holzwave",
    "~/Library/Caches/ch.holzcloud.holzwave",
    "~/Library/WebKit/ch.holzcloud.holzwave",
  ]

  caveats <<~EOS
    holzWave is unsigned (ad-hoc signature, no notarisation). The cask removes the quarantine attribute so that it opens.
  EOS
end
CASK
  }
  mkdir -p "$tap/Casks"
  write_cask "holzwave@preview" "$tap/Casks/holzwave@preview.rb"
  case "$version" in *-*) ;; *) write_cask "holzwave" "$tap/Casks/holzwave.rb" ;; esac
  echo "cask written into $tap/Casks: commit and push the tap"
fi
