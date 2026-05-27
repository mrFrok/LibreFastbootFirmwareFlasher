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

        # Runtime libraries needed by the prebuilt binary
        runtimeDeps = with pkgs; [
          fontconfig
          freetype
          libGL
          libxkbcommon
          wayland
          libx11
          libxcursor
          libxi
          libxrandr
          libxcb
          libxcb-util
          libxcb-keysyms
          libxcb-wm
          alsa-lib
          dbus
          openssl
          stdenv.cc.cc.lib
        ];

        # Build-time dependencies (only used when building from source)
        skiaSrc = pkgs.fetchFromGitHub {
          owner = "rust-skia";
          repo = "skia";
          rev = "m142-0.89.1";
          hash = "sha256-J7mBQ124/dODxX6MsuMW1NHizCMATAqdSzwxpP2afgk=";
        };

        commonRustArgs = {
          pname = "lfff";
          version = "2.3.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = with pkgs; [ pkg-config cmake makeWrapper installShellFiles python3 gn ninja ];
          buildInputs = runtimeDeps;
          PYTHON = "${pkgs.python3}/bin/python3";
          SKIA_SOURCE_DIR = "${skiaSrc}";
        };

        # Binary package — downloads prebuilt release from GitHub
        lfff-bin = pkgs.stdenv.mkDerivation rec {
          pname = "lfff-bin";
          version = "2.3.0";

          arch = if system == "aarch64-linux" then "aarch64" else "x86_64";

          src = pkgs.fetchurl {
            url = "https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/download/v${version}/lfff-gui-linux-${arch}.tar.gz";
            sha256 = if arch == "aarch64" then
              "sha256-6K6K12Gdip2F_IAeieTAM8h45_djKK1BQO94Z3aUmYA="
            else
              "sha256-wIPdT5UJ4RqjOexk17HiKtf-76klkRWE-u51tFFkIEw=";
          };

          nativeBuildInputs = with pkgs; [ makeWrapper ];

          unpackPhase = ''
            tar xzf $src
          '';

          installPhase = ''
            mkdir -p $out/bin $out/share/applications $out/share/icons/hicolor/scalable/apps
            install -Dm755 lfff-gui $out/bin/lfff-gui
            install -Dm644 ${./lfff-gui.desktop} $out/share/applications/lfff-gui.desktop
            install -Dm644 ${./lfff-gui.svg} $out/share/icons/hicolor/scalable/apps/lfff-gui.svg

            wrapProgram $out/bin/lfff-gui \
              --set FONTCONFIG_FILE ${pkgs.fontconfig.out}/etc/fonts/fonts.conf \
              --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath runtimeDeps}
          '';

          meta = with pkgs.lib; {
            description = "Android firmware flasher via fastboot (GUI, prebuilt binary)";
            homepage = "https://github.com/mrFrok/LibreFastbootFirmwareFlasher";
            license = licenses.gpl3;
            platforms = platforms.linux;
            mainProgram = "lfff-gui";
          };
        };

        # Build GUI package
        lfff-gui = pkgs.rustPlatform.buildRustPackage (commonRustArgs // {
          pname = "lfff-gui";
          cargoBuildFlags = [ "--package" "lfff-gui" ];
          
          postInstall = ''
            # Install CLI binary as well
            install -Dm755 target/release/lfff $out/bin/lfff
            
            # Install desktop file
            install -Dm644 lfff-gui.desktop $out/share/applications/lfff-gui.desktop
            
            # Install icon
            install -Dm644 lfff-gui.svg $out/share/icons/hicolor/scalable/apps/lfff-gui.svg
            
            # Wrap GUI binary with required environment
            wrapProgram $out/bin/lfff-gui \
              --set FONTCONFIG_FILE ${pkgs.fontconfig.out}/etc/fonts/fonts.conf \
              --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath runtimeDeps}
          '';

          meta = with pkgs.lib; {
            description = "Android firmware flasher via fastboot (GUI)";
            homepage = "https://github.com/mrFrok/LibreFastbootFirmwareFlasher";
            license = licenses.gpl3;
            platforms = platforms.linux;
            mainProgram = "lfff-gui";
          };
        });

        # Build CLI package
        lfff-cli = pkgs.rustPlatform.buildRustPackage (commonRustArgs // {
          pname = "lfff-cli";
          cargoBuildFlags = [ "--package" "lfff-cli" ];
          
          postInstall = ''
            install -Dm755 target/release/lfff $out/bin/lfff
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
              lfff-gui = lfff-gui;
              lfff-cli = lfff-cli;
            };
            linux-only = if system == "x86_64-linux" || system == "aarch64-linux" then
              { lfff-bin = lfff-bin; default = lfff-bin; }
            else
              { default = lfff-gui; };
          in
          common // linux-only;

        devShells.default = pkgs.mkShell {
          inputsFrom = [ lfff-gui ];
          
          packages = with pkgs; [
            rust-analyzer
            cargo-watch
            fastboot
            adb
            rustfmt
            clippy
          ];
        };

        apps =
          let
            gui-pkg = if system == "x86_64-linux" || system == "aarch64-linux" then lfff-bin else lfff-gui;
          in
          {
            default = {
              type = "app";
              program = "${gui-pkg}/bin/lfff-gui";
            };
            gui = {
              type = "app";
              program = "${gui-pkg}/bin/lfff-gui";
            };
            cli = {
              type = "app";
              program = "${lfff-cli}/bin/lfff";
            };
          };
      }
    );
}
