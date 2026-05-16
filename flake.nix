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
        
        # System dependencies required by Slint/Skia and the application
        systemDeps = with pkgs; [
          fontconfig
          freetype
          libGL
          libxkbcommon
          wayland
          xorg.libX11
          xorg.libXcursor
          xorg.libXi
          xorg.libXrandr
          xorg.libxcb
          xorg.xcbutil
          xorg.xcbutilkeysyms
          xorg.xcbutilwm
          alsa-lib
          dbus
          openssl
          pkg-config
        ];

        # Common Rust package arguments
        commonRustArgs = {
          pname = "lfff";
          version = "2.1.0";
          src = ./.;
          
          cargoLock.lockFile = ./Cargo.lock;
          
          nativeBuildInputs = with pkgs; [
            pkg-config
            cmake
            makeWrapper
            installShellFiles
            python3
            curl
          ];

          buildInputs = systemDeps;

          # Skia requires python3 during build.rs; Nix sandbox doesn't expose it via PATH automatically
          PYTHON = "${pkgs.python3}/bin/python3";
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
              --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath systemDeps}
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
        packages = {
          default = lfff-gui;
          lfff-gui = lfff-gui;
          lfff-cli = lfff-cli;
        };

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

        apps = {
          default = {
            type = "app";
            program = "${lfff-gui}/bin/lfff-gui";
          };
          gui = {
            type = "app";
            program = "${lfff-gui}/bin/lfff-gui";
          };
          cli = {
            type = "app";
            program = "${lfff-cli}/bin/lfff";
          };
        };
      }
    );
}
