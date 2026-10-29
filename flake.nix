{
  description = "A macOS-style top bar for niri, in GTK4";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    # Only for the VM tests, which run the shell through its home-manager module.
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      home-manager,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        inherit (self.packages.${system}) rust-wl-shell;
        # Runs `command` on the source offline, with the package's vendored dependencies.
        cargoCheck =
          name: tools: command:
          pkgs.stdenv.mkDerivation {
            name = "rust-wl-shell-${name}";
            inherit (rust-wl-shell) src cargoDeps buildInputs XKB_EVDEV_XML;
            nativeBuildInputs = [
              pkgs.rustPlatform.cargoSetupHook
              pkgs.cargo
              pkgs.pkg-config
            ]
            ++ tools;
            buildPhase = command;
            installPhase = "touch $out";
            dontFixup = true;
          };
      in
      {
        packages = rec {
          rust-wl-shell = pkgs.callPackage ./package.nix { };
          default = rust-wl-shell;
        };

        checks = {
          # niri on a virtio-gpu output in a VM, the bar started by the home-manager module,
          # checked by its pixels and its AT-SPI tree.
          bar = import ./tests/bar.nix {
            inherit pkgs;
            homeManager = home-manager.nixosModules.home-manager;
            module = self.homeManagerModules.default;
          };
          # The notification server, its banners and the history.
          notifications = import ./tests/notifications.nix {
            inherit pkgs;
            homeManager = home-manager.nixosModules.home-manager;
            module = self.homeManagerModules.default;
          };
          # The network, VPN and Bluetooth items, on virtual Wi-Fi radios and a fake BlueZ.
          network = import ./tests/network.nix {
            inherit pkgs;
            homeManager = home-manager.nixosModules.home-manager;
            module = self.homeManagerModules.default;
          };
          fmt = cargoCheck "fmt" [ pkgs.rustfmt ] "cargo fmt --all --check";
          clippy = cargoCheck "clippy" [
            pkgs.rustc
            pkgs.clippy
          ] "cargo clippy --workspace --all-targets -- -D warnings";
        };

        devShells.default = pkgs.mkShell {
          inputsFrom = [ rust-wl-shell ];
          inherit (rust-wl-shell) XKB_EVDEV_XML;
          packages = with pkgs; [
            rust-analyzer
            clippy
            rustfmt
          ];
        };
      }
    )
    // {
      homeManagerModules.default = ./module.nix;
    };
}
