{
  description = "Build a cargo project";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

    crane.url = "github:ipetkov/crane";

    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.rust-analyzer-src.follows = "";
    };

    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, crane, fenix, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        inherit (pkgs) lib;

        craneLib = crane.mkLib pkgs;
        src = lib.fileset.toSource {
          root = ./.;
          fileset = lib.fileset.unions [
            (craneLib.fileset.commonCargoSources ./.)
            # sql files are included in binary
            (lib.fileset.fileFilter (file: file.hasExt "sql") ./.)
          ];
        };

        commonArgs = with pkgs; {
          inherit src;
          strictDeps = true;
          buildInputs = [fuse];
          nativeBuildInputs = [
            pkg-config
            openssl
            perl # used by openssl-sys build script
          ];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        nightshift = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
        });
      in {
        checks = {
          inherit nightshift;
        };

        packages = {
          default = nightshift;
        };

        apps.default = flake-utils.lib.mkApp {
          drv = nightshift;
        };

        devShells.default = craneLib.devShell {
          checks = self.checks.${system};
          packages = with pkgs; [ rust-analyzer ];
        };
      });
}
