{
  description = "zoekt-mcp — MCP server wrapping Zoekt code search for Claude Code";

  # substrate.rust.tool dispatches over Cargo.gen.lock (the slim gen delta,
  # reconstructed to the full BuildSpec in pure Nix) — no crate2nix, no Cargo.nix.
  inputs.substrate.url = "github:pleme-io/substrate";
  # nixpkgs.lib is needed to build the HM helpers the module factory consumes.
  inputs.nixpkgs.follows = "substrate/nixpkgs";

  outputs = { substrate, nixpkgs, ... }:
    let
      base = substrate.rust.tool { src = ./.; };
    in
    # Re-attach the home-manager module export (services.zoekt.daemon/mcp) that the
    # bare `substrate.rust.tool` shape drops — nix/lib/hm-modules.nix consumes
    # `inputs.zoekt-mcp.homeManagerModules.default`, so dropping it breaks the
    # fleet's HM eval. (Recurring drop across the canonical-migration commits.)
    base // {
      homeManagerModules.default = import ./module {
        hmHelpers = import "${substrate}/lib/hm-service-helpers.nix" {
          lib = nixpkgs.lib;
        };
      };
    };
}
