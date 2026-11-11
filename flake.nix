{
  description = "Touchscreen edge-swipe brightness and volume adjuster for Wayland 2-in-1 laptops";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      supportedSystems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
      releaseMeta = builtins.fromJSON (builtins.readFile ./nix/release.json);
    in
    {
      packages = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          prebuiltInfo = releaseMeta.${system} or null;

          commonMeta = with pkgs.lib; {
            description = "Touchscreen edge-swipe brightness and volume daemon for Wayland 2-in-1s";
            license = licenses.mit;
            platforms = platforms.linux;
            mainProgram = "swipe-adjust";
          };

          swipe-adjust-source = pkgs.rustPlatform.buildRustPackage {
            pname = "swipe-adjust";
            version = releaseMeta.version or "0.1.0";
            src = self;

            cargoLock = {
              lockFile = ./Cargo.lock;
            };

            nativeBuildInputs = [
              pkgs.pkg-config
            ];

            buildInputs = [
              pkgs.libpulseaudio
            ];

            meta = commonMeta;
          };

          swipe-adjust-bin =
            if prebuiltInfo != null then
              pkgs.stdenv.mkDerivation {
                pname = "swipe-adjust";
                version = releaseMeta.version or "0.1.0";

                src = pkgs.fetchurl {
                  inherit (prebuiltInfo) url hash;
                };

                dontUnpack = true;

                nativeBuildInputs = [
                  pkgs.autoPatchelfHook
                ];

                buildInputs = [
                  pkgs.libpulseaudio
                  pkgs.stdenv.cc.cc.lib
                ];

                installPhase = ''
                  runHook preInstall
                  tar -xzf "$src"
                  install -Dm755 swipe-adjust/swipe-adjust "$out/bin/swipe-adjust"
                  runHook postInstall
                '';

                meta = commonMeta;
              }
            else
              null;

          swipe-adjust =
            if swipe-adjust-bin != null then swipe-adjust-bin else swipe-adjust-source;
        in
        {
          inherit swipe-adjust swipe-adjust-source;
          default = swipe-adjust;
        }
        // nixpkgs.lib.optionalAttrs (swipe-adjust-bin != null) {
          inherit swipe-adjust-bin;
        });

      devShells = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            nativeBuildInputs = with pkgs; [
              cargo
              rustc
              rustfmt
              clippy
              rust-analyzer
              pkg-config
            ];
            buildInputs = with pkgs; [
              libpulseaudio
            ];
          };
        });

      nixosModules = rec {
        swipe-adjust = import ./nix/nixos-module.nix;
        default = swipe-adjust;
      };

      homeManagerModules = rec {
        swipe-adjust = import ./nix/hm-module.nix { inherit self; };
        default = swipe-adjust;
      };

      checks = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          nixosEval = nixpkgs.lib.nixosSystem {
            inherit system;
            modules = [
              self.nixosModules.default
              {
                hardware.swipe-adjust.enable = true;
                boot.loader.grub.enable = false;
                fileSystems."/" = { device = "/dev/null"; fsType = "ext4"; };
                system.stateVersion = "25.11";
              }
            ];
          };
        in
        {
          package = self.packages.${system}.default;
          nixos-module =
            let
              matching = builtins.filter
                (p: (p.name or "") == "swipe-adjust-udev-rules")
                nixosEval.config.services.udev.packages;
              rulePkg = builtins.head matching;
            in
            pkgs.runCommand "swipe-adjust-udev-check" { } ''
              test -f "${rulePkg}/etc/udev/rules.d/70-swipe-adjust.rules"
              grep -q 'TAG+="uaccess"' "${rulePkg}/etc/udev/rules.d/70-swipe-adjust.rules"
              touch "$out"
            '';
        });
    };
}
