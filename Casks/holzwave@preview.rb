# The preview channel: the newest alpha, beta or plain release. Written by scripts/release-macos.sh.
cask "holzwave@preview" do
  version "0.0.1-beta2"
  sha256 "bfd13b7e03f77a8b41e5de33b414d9ef866db6a564e555a30a2aa48af6b6afb7"

  url "https://github.com/holzcloud/holzWave/releases/download/v#{version}/holzWave_#{version}_aarch64.dmg"
  name "holzWave"
  desc "Terminal emulator with tiled panes, tabs and workspaces"
  homepage "https://holzcloud.ch/holzwave"

  livecheck do
    url "https://github.com/holzcloud/holzWave.git"
    strategy :git
    regex(/^v?(\d+(?:\.\d+)+(?:-(?:alpha|beta)\d+)?)$/i)
  end

  conflicts_with cask: "holzwave"
  depends_on arch: :arm64
  depends_on macos: :ventura

  app "holzWave.app"

  # The app is signed ad hoc only, so Gatekeeper would refuse to open it while it carries the quarantine attribute.
  postflight_steps do
    run "/usr/bin/xattr",
        args:           ["-dr", "com.apple.quarantine", "{{appdir}}/holzWave.app"],
        must_succeed:   false,
        writable_paths: ["holzWave.app"],
        writable_base:  :appdir
  end

  uninstall quit: "ch.holzcloud.holzwave"

  zap trash: [
    "~/Library/Application Support/ch.holzcloud.holzwave",
    "~/Library/Caches/ch.holzcloud.holzwave",
    "~/Library/WebKit/ch.holzcloud.holzwave",
  ]

  caveats <<~EOS
    holzWave is signed ad hoc only and is not notarized by Apple.
    This cask removes the quarantine attribute after the install so that macOS opens the app:
    that bypasses a protection of macOS. If you do not accept that, do not use this cask;
    the DMG is on the releases page.
    It is not in the official Homebrew cask repository, which does not take apps that fail Gatekeeper.
    Apple Silicon only. Published by holzcloud.ch.
    This is a pre-release.
  EOS
end
