# The preview channel: the newest alpha, beta or plain release. Written by scripts/release-macos.sh.
cask "holzwave@preview" do
  version "0.0.1-alpha3"
  sha256 "fd56b4fdbf9dedc349ae2dbac8ff4408f123f9cdad7fdb00b3dc3c9b9be2e6d4"

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
end
