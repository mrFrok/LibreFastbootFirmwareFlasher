class Lfff < Formula
  desc "Free, open-source firmware flasher for Android A/B devices via fastboot"
  homepage "https://github.com/mrFrok/LibreFastbootFirmwareFlasher"
  version "2.9.4"

  url "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v2.9.4/lfff-macos-aarch64.tar.gz"
  sha256 "f850d01ac3533d802beab06bbd8f59061b78e2ff341f8a136482c0b40c86dafd"

  resource "cli" do
    on_macos do
      on_arm do
        url "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v2.9.4/lfff-macos-aarch64.tar.gz"
        sha256 "f850d01ac3533d802beab06bbd8f59061b78e2ff341f8a136482c0b40c86dafd"
      end
      on_intel do
        url "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v2.9.4/lfff-macos-x86_64.tar.gz"
        sha256 "e653f1245214079447fade00f5a0752dabd3e5bb6335213fcbf9b2e1869d64dc"
      end
    end
    on_linux do
      on_arm do
        url "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v2.9.4/lfff-linux-aarch64.tar.gz"
        sha256 "2e6275e1e00ad5317e3076996d706005bcc836992eed28208d3484092e9abb7d"
      end
      on_intel do
        url "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v2.9.4/lfff-linux-x86_64.tar.gz"
        sha256 "9d8d33f40a70b2544f15c0a3758985a55c6cb9afcbe3ecf8203d1f74aa347429"
      end
    end
  end

  resource "gui" do
    on_macos do
      on_arm do
        url "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v2.9.4/lfff-gui-macos-aarch64.tar.gz"
        sha256 "8829e570e161274e21e7317d1229ac6684ddbc2a7ea7e1bb76657b333384e2f4"
      end
      on_intel do
        url "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v2.9.4/lfff-gui-macos-x86_64.tar.gz"
        sha256 "02631fce490909ed7894d09eda63c740f8e1f3340060eaa5bf629ec85dabdb20"
      end
    end
    on_linux do
      on_arm do
        url "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v2.9.4/lfff-gui-linux-aarch64.tar.gz"
        sha256 "a521fca51c310b593b3fa0a3f809bea5b4bbe53a9da4fc0a4459951213a7ab93"
      end
      on_intel do
        url "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v2.9.4/lfff-gui-linux-x86_64.tar.gz"
        sha256 "55b3f58f5c7351ad90d999e7df105ac690243ebf2a0863e2ab6c59150daa0c78"
      end
    end
  end

  def install
    resource("cli").stage { bin.install "lfff" }
    # On macOS the app comes from the cask, which also pulls in this formula:
    # installing it here too would clash over bin/lfff-gui.
    unless OS.mac?
      resource("gui").stage { bin.install "lfff-gui" }
    end

    generate_completions_from_executable(bin/"lfff", "completion")
  end

  def caveats
    if OS.mac?
      <<~EOS
        This formula is the command-line tool. For the app:
          brew install --cask librefastbootfirmwareflasher
      EOS
    end
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/lfff --version")
    assert_predicate bin/"lfff-gui", :exist? unless OS.mac?
  end
end
