{
  description = "zoekt-mcp — MCP server wrapping Zoekt code search for Claude Code";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    substrate = {
      url = "git+ssh://git@github.com/pleme-io/substrate.git";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, substrate, ... }:
    flake-utils.lib.eachDefaultSystem (system: let
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ substrate.overlays.${system}.rust ];
      };

      zoekt-mcp = pkgs.rustPlatform.buildRustPackage {
        pname = "zoekt-mcp";
        version = "0.1.0";
        src = ./.;
        cargoLock.lockFile = ./Cargo.lock;

        nativeBuildInputs = [ pkgs.pkg-config ];
        buildInputs = pkgs.lib.optionals pkgs.stdenv.isDarwin [
          pkgs.libiconv
        ] ++ (
          if pkgs ? apple-sdk
          then [ pkgs.apple-sdk ]
          else pkgs.lib.optionals (pkgs ? darwin) (
            with pkgs.darwin.apple_sdk.frameworks; [
              Security
              SystemConfiguration
            ]
          )
        );

        meta = with pkgs.lib; {
          description = "MCP server wrapping Zoekt code search for Claude Code";
          homepage = "https://github.com/pleme-io/zoekt-mcp";
          license = licenses.mit;
          mainProgram = "zoekt-mcp";
        };
      };
    in {
      packages = {
        default = zoekt-mcp;
        zoekt-mcp = zoekt-mcp;
      };

      apps.default = {
        type = "app";
        program = "${zoekt-mcp}/bin/zoekt-mcp";
      };

      devShells.default = pkgs.mkShell {
        nativeBuildInputs = [
          pkgs.cargo
          pkgs.rustc
          pkgs.pkg-config
        ];
        buildInputs = pkgs.lib.optionals pkgs.stdenv.isDarwin [
          pkgs.libiconv
        ] ++ (
          if pkgs ? apple-sdk
          then [ pkgs.apple-sdk ]
          else pkgs.lib.optionals (pkgs ? darwin) (
            with pkgs.darwin.apple_sdk.frameworks; [
              Security
              SystemConfiguration
            ]
          )
        );
        RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
      };
    }) // {
      # Non-per-system outputs
      homeManagerModules.default = import ./module {
        hmHelpers = import "${substrate}/lib/hm-service-helpers.nix" { lib = nixpkgs.lib; };
      };
      overlays.default = final: prev: {
        zoekt-mcp = self.packages.${final.system}.default;
      };
    };
}
