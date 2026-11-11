{
  description = "Touchscreen edge-swipe brightness and volume adjuster for Wayland 2-in-1 laptops";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      supportedSystems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
    in
    {
      packages = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        rec {
          swipe-adjust = pkgs.rustPlatform.buildRustPackage {
            pname = "swipe-adjust";
            version = "0.1.0";
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

            meta = with pkgs.lib; {
              description = "Touchscreen edge-swipe brightness and volume daemon for Wayland 2-in-1s";
              license = licenses.mit;
              platforms = platforms.linux;
              mainProgram = "swipe-adjust";
            };
          };

          default = swipe-adjust;
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
          nixos-module = pkgs.writeText "swipe-adjust-udev-check" nixosEval.config.services.udev.extraRules;
        });
    };
}
