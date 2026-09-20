class Lfff < Formula
  desc "Free, open-source firmware flasher for Android A/B devices via fastboot"
  homepage "https://github.com/mrFrok/LibreFastbootFirmwareFlasher"
  version "2.7.2"

  url "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v2.7.2/lfff-macos-aarch64.tar.gz"
  sha256 "c6beb3f03714b24eb7570e9587dc9cde24993056e78e0a26121440e3179aa285"

  resource "cli" do
    on_macos do
      on_arm do
        url "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v2.7.2/lfff-macos-aarch64.tar.gz"
        sha256 "c6beb3f03714b24eb7570e9587dc9cde24993056e78e0a26121440e3179aa285"
      end
      on_intel do
        url "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v2.7.2/lfff-macos-x86_64.tar.gz"
        sha256 "0c0b88e40a54c9c1db2d611b1d2cf8d541adc508c5fa853656efe1c861c1d015"
      end
    end
    on_linux do
      on_arm do
        url "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v2.7.2/lfff-linux-aarch64.tar.gz"
        sha256 "8aecd8bc6277e31bfa3f375103dde1a93298b4bfde707cd0ace4a6877a9a9116"
      end
      on_intel do
        url "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v2.7.2/lfff-linux-x86_64.tar.gz"
        sha256 "e6d47e6701be1521526d3a8062c0d4f3e3d4793ebd603e2ea71ab38d6bd0fac5"
      end
    end
  end

  resource "gui" do
    on_macos do
      on_arm do
        url "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v2.7.2/lfff-gui-macos-aarch64.tar.gz"
        sha256 "e107a466cb127b142b940d896ec913a19a043dea9cda6449eadd8bcc91031340"
      end
      on_intel do
        url "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v2.7.2/lfff-gui-macos-x86_64.tar.gz"
        sha256 "f7560e965e8e656be6ce3b2c07f501ad3b7b4e175325b99f232a0afdc87c433f"
      end
    end
    on_linux do
      on_arm do
        url "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v2.7.2/lfff-gui-linux-aarch64.tar.gz"
        sha256 "db2e5d3b85f20ced99954f4a7ad0310e0b26c26b8962e2dcae84b75440a5eb15"
      end
      on_intel do
        url "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v2.7.2/lfff-gui-linux-x86_64.tar.gz"
        sha256 "72a5c56f974f7816842b8496f3ddaa467a02d01ec9c6415d53f422759dba28d2"
      end
    end
  end

  def install
    resource("cli").stage { bin.install "lfff" }
    resource("gui").stage do |stage|
      if OS.mac? && (stage + "LibreFastbootFirmwareFlasher.app").exist?
        bin.install "LibreFastbootFirmwareFlasher.app/Contents/MacOS/lfff-gui"
        prefix.install "LibreFastbootFirmwareFlasher.app"
      else
        bin.install "lfff-gui"
      end
    end

    generate_completions_from_executable(bin/"lfff", "completion")
  end

  def caveats
    if OS.mac?
      <<~EOS
        LibreFastbootFirmwareFlasher.app is installed in the Cellar. To use it from Launchpad / Finder:
          cp -r #{prefix}/LibreFastbootFirmwareFlasher.app /Applications
      EOS
    end
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/lfff --version")
    assert_predicate bin/"lfff-gui", :exist?
  end
end
