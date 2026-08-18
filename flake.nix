{
  description = "LibreFastbootFirmwareFlasher - Android firmware flasher via fastboot";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        version = "2.7.0";
        arch = if system == "aarch64-linux" then "aarch64" else "x86_64";

        buildInputs = with pkgs; [
          fontconfig freetype libGL libxkbcommon wayland libx11 libxcursor
          libxi libxrandr libxcb libxcb-util libxcb-keysyms libxcb-wm
          alsa-lib dbus openssl stdenv.cc.cc.lib
        ];

        assets = pkgs.fetchFromGitHub {
          owner = "mrFrok";
          repo = "LibreFastbootFirmwareFlasher";
          rev = "v${version}";
          hash = "sha256-7dVCuNmLkLGP2Z4RSK09YNHOoe7D4RJnGJjKOdnXZOA=";
        };
      in
      {
        packages = {
          lfff-gui = pkgs.stdenvNoCC.mkDerivation {
            pname = "lfff-gui";
            inherit version;

            src = pkgs.fetchurl {
              url = "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v${version}/lfff-gui-linux-${arch}.tar.gz";
              sha256 = if arch == "aarch64" then
                "sha256-rWUFhnQfQgEHWgMJ4LFxGG0zA/6S6MSITM9dSgAQ8E8="
              else
                "sha256-6QdfbfyrSXxx0FV8nlhvSX+wlB05UndUVj9xyqRYVdg=";
            };

            nativeBuildInputs = with pkgs; [ autoPatchelfHook makeWrapper ];
            inherit buildInputs;

            unpackPhase = "tar xzf $src";

            installPhase = ''
              runHook preInstall
              mkdir -p $out/bin $out/share/applications $out/share/icons/hicolor/scalable/apps

              install -Dm755 lfff-gui $out/bin/lfff-gui
              install -Dm644 ${assets}/lfff-gui.desktop $out/share/applications/lfff-gui.desktop
              install -Dm644 ${assets}/lfff-gui.svg $out/share/icons/hicolor/scalable/apps/lfff-gui.svg

              wrapProgram $out/bin/lfff-gui \
                --set FONTCONFIG_FILE ${pkgs.fontconfig.out}/etc/fonts/fonts.conf \
                --set SLINT_BACKEND wayland \
                --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath buildInputs} \
                --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.payload-dumper-go pkgs.aria2 pkgs.android-tools ]}

              runHook postInstall
            '';

            meta = with pkgs.lib; {
              description = "Android firmware flasher via fastboot (GUI, prebuilt)";
              homepage = "https://github.com/mrFrok/LibreFastbootFirmwareFlasher";
              license = licenses.gpl3;
              platforms = platforms.linux;
              mainProgram = "lfff-gui";
              sourceProvenance = with sourceTypes; [ binaryNativeCode ];
            };
          };

          lfff-cli = pkgs.stdenvNoCC.mkDerivation {
            pname = "lfff-cli";
            inherit version;

            src = pkgs.fetchurl {
              url = "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v${version}/lfff-linux-${arch}.tar.gz";
              hash = "sha256-TKyh54YRUxQOFxryewHI6SlNunch4cKOYDGglN3RtOI=";
            };

            buildInputs = with pkgs; [ stdenv.cc.cc.lib ];
            nativeBuildInputs = with pkgs; [ autoPatchelfHook ];

            unpackPhase = "tar xzf $src";

            installPhase = ''
              runHook preInstall
              mkdir -p $out/bin
              install -Dm755 lfff $out/bin/lfff
              runHook postInstall
            '';

            meta = with pkgs.lib; {
              description = "Android firmware flasher via fastboot (CLI, prebuilt)";
              homepage = "https://github.com/mrFrok/LibreFastbootFirmwareFlasher";
              license = licenses.gpl3;
              platforms = platforms.linux;
              mainProgram = "lfff";
              sourceProvenance = with sourceTypes; [ binaryNativeCode ];
            };
          };

          default = self.packages.${system}.lfff-gui;
        };

        apps = {
          default = flake-utils.lib.mkApp { drv = self.packages.${system}.lfff-gui; exePath = "/bin/lfff-gui"; };
          gui = flake-utils.lib.mkApp { drv = self.packages.${system}.lfff-gui; exePath = "/bin/lfff-gui"; };
          cli = flake-utils.lib.mkApp { drv = self.packages.${system}.lfff-cli; exePath = "/bin/lfff"; };
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [ fastboot adb payload-dumper-go aria2 ];
        };
      }
    );
}
