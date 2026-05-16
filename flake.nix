{
  description = "LibreFastbootFirmwareFlasher - Android firmware flasher via fastboot";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, crane }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        craneLib = crane.mkLib pkgs;

        # Common arguments for crane
        commonArgs = {
          src = craneLib.cleanCargoSource ./.;
          strictDeps = true;
          
          buildInputs = with pkgs; [
            # Slint/Skia dependencies
            fontconfig
            freetype
            libGL
            libxkbcommon
            wayland
            xorg.libX11
            xorg.libXcursor
            xorg.libXi
            xorg.libXrandr
          ];

          nativeBuildInputs = with pkgs; [
            pkg-config
            cmake
          ];
        };

        # Build the workspace
        workspace = craneLib.buildWorkspace (commonArgs // {
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        });

        # GUI package with desktop integration
        lfff-gui = pkgs.stdenv.mkDerivation {
          pname = "lfff-gui";
          version = "2.0.5";
          
          src = ./.;
          
          nativeBuildInputs = with pkgs; [
            installShellFiles
            makeWrapper
            pkg-config
            cmake
            rustPlatform.cargoSetupHook
            cargo
            rustc
          ];

          buildInputs = with pkgs; [
            fontconfig
            freetype
            libGL
            libxkbcommon
            wayland
            xorg.libX11
            xorg.libXcursor
            xorg.libXi
            xorg.libXrandr
          ];

          cargoDeps = craneLib.fetchCargoTarball {
            inherit (commonArgs) src;
          };

          buildPhase = ''
            runHook preBuild
            cargo build --release --package lfff-gui --package lfff-cli
            runHook postBuild
          '';

          installPhase = ''
            runHook preInstall
            
            # Install binaries
            install -Dm755 target/release/lfff-gui $out/bin/lfff-gui
            install -Dm755 target/release/lfff $out/bin/lfff
            
            # Install desktop file
            install -Dm644 lfff-gui.desktop $out/share/applications/lfff-gui.desktop
            
            # Install icon
            install -Dm644 lfff-gui.svg $out/share/icons/hicolor/scalable/apps/lfff-gui.svg
            
            # Wrap GUI binary with required libraries
            wrapProgram $out/bin/lfff-gui \
              --set FONTCONFIG_FILE ${pkgs.fontconfig.out}/etc/fonts/fonts.conf \
              --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath [
                pkgs.fontconfig
                pkgs.freetype
                pkgs.libGL
                pkgs.libxkbcommon
                pkgs.wayland
                pkgs.xorg.libX11
                pkgs.xorg.libXcursor
                pkgs.xorg.libXi
                pkgs.xorg.libXrandr
              ]}
            
            runHook postInstall
          '';

          meta = with pkgs.lib; {
            description = "Android firmware flasher via fastboot";
            homepage = "https://github.com/mrFrok/LibreFastbootFirmwareFlasher";
            license = licenses.gpl3;
            platforms = platforms.linux;
            mainProgram = "lfff-gui";
          };
        };
      in
      {
        packages = {
          default = lfff-gui;
          lfff-gui = lfff-gui;
          lfff-cli = workspace;
        };

        devShells.default = pkgs.mkShell {
          inputsFrom = [ workspace ];
          
          packages = with pkgs; [
            rust-analyzer
            cargo-watch
            fastboot
            adb
          ];
        };

        apps = {
          default = {
            type = "app";
            program = "${lfff-gui}/bin/lfff-gui";
          };
        };
      }
    );
}
