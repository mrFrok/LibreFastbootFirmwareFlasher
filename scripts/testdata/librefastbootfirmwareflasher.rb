cask "librefastbootfirmwareflasher" do
  arch arm: "aarch64", intel: "x86_64"

  version "2.9.4"
  sha256 arm:   "8829e570e161274e21e7317d1229ac6684ddbc2a7ea7e1bb76657b333384e2f4",
         intel: "02631fce490909ed7894d09eda63c740f8e1f3340060eaa5bf629ec85dabdb20"

  url "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v#{version}/lfff-gui-macos-#{arch}.tar.gz"
  name "Libre Fastboot Firmware Flasher"
  desc "Free, open-source firmware flasher for Android A/B devices via fastboot"
  homepage "https://github.com/mrFrok/LibreFastbootFirmwareFlasher"

  # The command-line tool comes from the formula of this tap.
  depends_on formula: "mrfrok/lfff/lfff"

  app "LibreFastbootFirmwareFlasher.app"
  binary "#{appdir}/LibreFastbootFirmwareFlasher.app/Contents/MacOS/lfff-gui"

  # The app is not notarised; with the quarantine flag Gatekeeper refuses to
  # open it at all.
  postflight do
    system_command "/usr/bin/xattr",
                   args: ["-dr", "com.apple.quarantine", "#{appdir}/LibreFastbootFirmwareFlasher.app"]
  end

  zap trash: "~/Library/Application Support/lfff"
end
