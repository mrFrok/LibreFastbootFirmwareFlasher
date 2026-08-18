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
        lib = pkgs.lib;

        version = "2.7.0";
        isLinux = pkgs.stdenv.hostPlatform.isLinux;
        arch = if pkgs.stdenv.hostPlatform.isAarch64 then "aarch64" else "x86_64";
        os = if pkgs.stdenv.hostPlatform.isDarwin then "macos" else "linux";

        runtimeDeps = with pkgs; [
          fontconfig freetype libGL libxkbcommon wayland libx11 libxcursor
          libxi libxrandr libxcb libxcb-util libxcb-keysyms libxcb-wm
          alsa-lib dbus openssl stdenv.cc.cc.lib
        ];

        payloadDumperVersion = "0.8.4";
        payload-dumper-rust = pkgs.stdenvNoCC.mkDerivation {
          pname = "payload-dumper-rust";
          version = payloadDumperVersion;

          src = pkgs.fetchurl {
            url = "https://github.com/rhythmcache/payload-dumper-rust/releases/download/payload-dumper-rust-v${payloadDumperVersion}/payload_dumper-${os}-${arch}.zip";
            hash = {
              linux-x86_64 = "sha256-0qfPTY702qc+vTTgxvd+tsEWoXRaGzGtiX6c4rL1QVE=";
              linux-aarch64 = "sha256-+5WoJZEGAfDQfHLxthS/c2d05k5h5v6n2pjk3sW7ojg=";
              macos-x86_64 = "sha256-CBFVM9eUIOKoL/a7xePwJoA1jNfMdyLB9F7urEjYGts=";
              macos-aarch64 = "sha256-sM9SHt2KtDsNMuSyq3CyY1q8VZ5AVQRbNdt2MmEaEiI=";
            }.${os + "-" + arch};
          };

          nativeBuildInputs = [ pkgs.unzip ];
          sourceRoot = ".";

          installPhase = ''
            runHook preInstall
            install -Dm755 payload_dumper $out/bin/payload_dumper
            runHook postInstall
          '';

          meta = with pkgs.lib; {
            description = "Fast Android OTA payload dumper (prebuilt binary)";
            homepage = "https://github.com/rhythmcache/payload-dumper-rust";
            license = licenses.asl20;
            mainProgram = "payload_dumper";
            sourceProvenance = with sourceTypes; [ binaryNativeCode ];
          };
        };

        runtimeTools = [ payload-dumper-rust pkgs.aria2 pkgs.android-tools ];

        wrapGui = ''
          wrapProgram $out/bin/lfff-gui \
            --set FONTCONFIG_FILE ${pkgs.fontconfig.out}/etc/fonts/fonts.conf \
            --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath runtimeDeps} \
            --prefix PATH : ${pkgs.lib.makeBinPath runtimeTools}
        '';

        skiaSrc = pkgs.fetchFromGitHub {
          owner = "rust-skia";
          repo = "skia";
          rev = "m142-0.89.1";
          hash = "sha256-J7mBQ124/dODxX6MsuMW1NHizCMATAqdSzwxpP2afgk=";
        };

        commonRustArgs = {
          pname = "lfff";
          inherit version;
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = with pkgs; [ pkg-config cmake makeWrapper installShellFiles python3 gn ninja ];
          buildInputs = runtimeDeps;
          PYTHON = "${pkgs.python3}/bin/python3";
          SKIA_SOURCE_DIR = "${skiaSrc}";
        };

        lfff-bin = pkgs.stdenvNoCC.mkDerivation {
          pname = "lfff-bin";
          inherit version;

          src = pkgs.fetchurl {
            url = "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v${version}/lfff-gui-${os}-${arch}.tar.gz";
            hash = {
              linux-x86_64 = "sha256-6QdfbfyrSXxx0FV8nlhvSX+wlB05UndUVj9xyqRYVdg=";
              linux-aarch64 = "sha256-rWUFhnQfQgEHWgMJ4LFxGG0zA/6S6MSITM9dSgAQ8E8=";
            }.${os + "-" + arch};
          };

          nativeBuildInputs = with pkgs; [ autoPatchelfHook makeWrapper ];
          buildInputs = runtimeDeps;

          unpackPhase = "tar xzf $src";

          installPhase = ''
            runHook preInstall
            install -Dm755 lfff-gui $out/bin/lfff-gui
            install -Dm644 ${./lfff-gui.desktop} $out/share/applications/lfff-gui.desktop
            install -Dm644 ${./lfff-gui.svg} $out/share/icons/hicolor/scalable/apps/lfff-gui.svg
            runHook postInstall
          '';

          postFixup = wrapGui;

          meta = with pkgs.lib; {
            description = "Android firmware flasher via fastboot (GUI, prebuilt binary)";
            homepage = "https://github.com/mrFrok/LibreFastbootFirmwareFlasher";
            license = licenses.gpl3;
            platforms = platforms.linux;
            mainProgram = "lfff-gui";
            sourceProvenance = with sourceTypes; [ binaryNativeCode ];
          };
        };

        lfff-cli-bin = pkgs.stdenvNoCC.mkDerivation {
          pname = "lfff-cli-bin";
          inherit version;

          src = pkgs.fetchurl {
            url = "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v${version}/lfff-${os}-${arch}.tar.gz";
            hash = {
              linux-x86_64 = "sha256-TKyh54YRUxQOFxryewHI6SlNunch4cKOYDGglN3RtOI=";
              linux-aarch64 = "sha256-1wDHJQQoRZ+8pxTKji6fUp/a8g1LHoKVySo01rcAfWw=";
            }.${os + "-" + arch};
          };

          nativeBuildInputs = with pkgs; [ autoPatchelfHook makeWrapper ];
          buildInputs = with pkgs; [ stdenv.cc.cc.lib ];

          unpackPhase = "tar xzf $src";

          installPhase = ''
            runHook preInstall
            install -Dm755 lfff $out/bin/lfff
            runHook postInstall
          '';

          postFixup = ''
            wrapProgram $out/bin/lfff \
              --prefix PATH : ${pkgs.lib.makeBinPath runtimeTools}
          '';

          meta = with pkgs.lib; {
            description = "Android firmware flasher via fastboot (CLI, prebuilt binary)";
            homepage = "https://github.com/mrFrok/LibreFastbootFirmwareFlasher";
            license = licenses.gpl3;
            platforms = platforms.linux;
            mainProgram = "lfff";
            sourceProvenance = with sourceTypes; [ binaryNativeCode ];
          };
        };

        lfff-gui = pkgs.rustPlatform.buildRustPackage (commonRustArgs // {
          pname = "lfff-gui";
          cargoBuildFlags = [ "--package" "lfff-gui" ];
          postInstall = ''
            install -Dm755 target/release/lfff $out/bin/lfff
            install -Dm644 lfff-gui.desktop $out/share/applications/lfff-gui.desktop
            install -Dm644 lfff-gui.svg $out/share/icons/hicolor/scalable/apps/lfff-gui.svg
          '' + wrapGui;

          meta = with pkgs.lib; {
            description = "Android firmware flasher via fastboot (GUI)";
            homepage = "https://github.com/mrFrok/LibreFastbootFirmwareFlasher";
            license = licenses.gpl3;
            platforms = platforms.linux;
            mainProgram = "lfff-gui";
          };
        });

        lfff-cli = pkgs.rustPlatform.buildRustPackage (commonRustArgs // {
          pname = "lfff-cli";
          cargoBuildFlags = [ "--package" "lfff-cli" ];
          postInstall = ''
            install -Dm755 target/release/lfff $out/bin/lfff
            wrapProgram $out/bin/lfff \
              --prefix PATH : ${pkgs.lib.makeBinPath runtimeTools}
          '';

          meta = with pkgs.lib; {
            description = "Android firmware flasher via fastboot (CLI)";
            homepage = "https://github.com/mrFrok/LibreFastbootFirmwareFlasher";
            license = licenses.gpl3;
            platforms = platforms.linux;
            mainProgram = "lfff";
          };
        });

      in
      {
        packages =
          let
            common = {
              inherit lfff-gui lfff-cli payload-dumper-rust;
            };
            linux-only = lib.optionalAttrs isLinux {
              inherit lfff-bin lfff-cli-bin;
              default = lfff-bin;
            };
            other = lib.optionalAttrs (!isLinux) {
              default = lfff-gui;
            };
          in
          common // linux-only // other;

        devShells.default = pkgs.mkShell {
          inputsFrom = [ lfff-gui ];
          packages = (with pkgs; [
            rust-analyzer
            cargo-watch
            rustfmt
            clippy
            android-tools
            aria2
          ]) ++ [ payload-dumper-rust ];
        };

        apps =
          let
            gui-pkg = if isLinux then lfff-bin else lfff-gui;
            cli-pkg = if isLinux then lfff-cli-bin else lfff-cli;
          in
          {
            default = { type = "app"; program = "${gui-pkg}/bin/lfff-gui"; };
            gui = { type = "app"; program = "${gui-pkg}/bin/lfff-gui"; };
            cli = { type = "app"; program = "${cli-pkg}/bin/lfff"; };
          };
      }
    );
}
