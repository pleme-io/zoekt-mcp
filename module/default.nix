# Zoekt home-manager module — daemon (single process) + MCP server entry
#
# Namespace: services.zoekt.daemon.* / services.zoekt.mcp.*
#
# The daemon reads a YAML config and manages zoekt-webserver + zoekt-git-index
# as child processes (mirroring the codesearch daemon architecture).
#
# Zoekt itself comes from nixpkgs (Go package). zoekt-mcp (this flake's package)
# is the Rust binary providing both the MCP server and the daemon.
#
# Module factory: receives { hmHelpers } from flake.nix, returns HM module.
{ hmHelpers }:
{
  lib,
  config,
  pkgs,
  ...
}:
with lib; let
  inherit (hmHelpers) mkMcpOptions mkMcpServerEntry mkAnvilRegistration mkLaunchdService mkSystemdService;
  daemonCfg = config.services.zoekt.daemon;
  mcpCfg = config.services.zoekt.mcp;
  ctagsCfg = daemonCfg.ctags;
  githubCfg = daemonCfg.github;
  webCfg = daemonCfg.webserver;
  isDarwin = pkgs.stdenv.isDarwin;

  logDir = if isDarwin
    then "${config.home.homeDirectory}/Library/Logs"
    else "${config.home.homeDirectory}/.local/share/zoekt/logs";

  # ── Daemon YAML config (generated from nix options) ──────────────────
  zoektDaemonConfig = pkgs.writeText "zoekt-daemon.yaml"
    (builtins.toJSON ({
      port = daemonCfg.port;
      index_dir = daemonCfg.indexDir;
      index_interval = daemonCfg.indexInterval;
      zoekt_bin = "${daemonCfg.package}/bin";
      git_bin = "${pkgs.git}/bin";
      delta = daemonCfg.delta;
      branches = daemonCfg.branches;
      parallelism = daemonCfg.parallelism;
      file_limit = daemonCfg.fileLimit;
      large_files = daemonCfg.largeFiles;
      repos = daemonCfg.repos;
      ctags = {
        enable = ctagsCfg.enable;
        require = ctagsCfg.require;
      };
      webserver = {
        rpc = webCfg.rpc;
        html = webCfg.html;
        pprof = webCfg.pprof;
        log_dir = webCfg.logDir;
        log_refresh = webCfg.logRefresh;
      };
    }
    // optionalAttrs ctagsCfg.enable {
      ctags_bin = "${ctagsCfg.package}/bin";
    }
    // optionalAttrs githubCfg.enable {
      github = {
        sources = map (s: {
          owner = s.owner;
          kind = s.kind;
          clone_base = s.cloneBase;
          auto_clone = s.autoClone;
          skip_archived = s.skipArchived;
          skip_forks = s.skipForks;
          exclude = s.exclude;
        }) githubCfg.sources;
      } // optionalAttrs (githubCfg.tokenFile != null) {
        token_file = githubCfg.tokenFile;
      };
    }));
in {
  options.services.zoekt = {
    # ── Daemon options ─────────────────────────────────────────────────
    daemon = {
      enable = mkOption {
        type = types.bool;
        default = false;
        description = "Enable Zoekt code search daemon (trigram-indexed search)";
      };

      package = mkOption {
        type = types.package;
        default = pkgs.zoekt;
        description = "Zoekt package providing zoekt-webserver and zoekt-git-index";
      };

      ctags = {
        enable = mkOption {
          type = types.bool;
          default = true;
          description = "Enable universal-ctags for symbol extraction (enables sym: queries)";
        };

        package = mkOption {
          type = types.package;
          default = pkgs.universal-ctags;
          description = "universal-ctags package";
        };

        require = mkOption {
          type = types.bool;
          default = true;
          description = "If true, ctags calls must succeed (-require_ctags). Set false to allow partial indexing.";
        };
      };

      repos = mkOption {
        type = types.listOf types.str;
        default = [];
        description = "Git repository paths to index (e.g. [\"/home/user/code/myrepo\"])";
      };

      indexDir = mkOption {
        type = types.str;
        default = "${config.home.homeDirectory}/.zoekt/index";
        description = "Directory for Zoekt index shards";
      };

      port = mkOption {
        type = types.int;
        default = 6070;
        description = "Zoekt webserver listen port";
      };

      indexInterval = mkOption {
        type = types.int;
        default = 300;
        description = "Re-index interval in seconds";
      };

      delta = mkOption {
        type = types.bool;
        default = true;
        description = "Only re-index changed files (-delta). Dramatically faster incremental updates.";
      };

      branches = mkOption {
        type = types.str;
        default = "HEAD";
        description = "Comma-separated branch list to index (-branches). Default HEAD indexes only the checked-out branch.";
      };

      largeFiles = mkOption {
        type = types.listOf types.str;
        default = [];
        description = "Glob patterns for files to index regardless of size (-large_file per entry).";
      };

      parallelism = mkOption {
        type = types.int;
        default = 4;
        description = "Number of concurrent indexing processes (-parallelism).";
      };

      fileLimit = mkOption {
        type = types.int;
        default = 2097152;
        description = "Maximum file size in bytes to index (-file_limit). Default 2 MiB matches upstream.";
      };

      github = {
        enable = mkOption {
          type = types.bool;
          default = false;
          description = "Enable GitHub auto-discovery of repos (list org/user repos, resolve to local clones)";
        };

        tokenFile = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "Path to file containing GitHub token. Supports ~ for home dir. Falls back to GITHUB_TOKEN env var.";
        };

        sources = mkOption {
          type = types.listOf (types.submodule {
            options = {
              owner = mkOption {
                type = types.str;
                description = "GitHub owner name (org or username)";
              };
              kind = mkOption {
                type = types.enum ["org" "user"];
                default = "org";
                description = "Whether this is an organization or user account";
              };
              cloneBase = mkOption {
                type = types.str;
                description = "Local directory where repos are/should be cloned";
              };
              autoClone = mkOption {
                type = types.bool;
                default = false;
                description = "Automatically clone repos that don't exist locally";
              };
              skipArchived = mkOption {
                type = types.bool;
                default = true;
                description = "Skip archived repositories";
              };
              skipForks = mkOption {
                type = types.bool;
                default = false;
                description = "Skip forked repositories";
              };
              exclude = mkOption {
                type = types.listOf types.str;
                default = [];
                description = "Glob patterns to exclude repo names (e.g. [\"*.wiki\" \"legacy-*\"])";
              };
            };
          });
          default = [];
          description = "GitHub sources to discover repos from";
        };
      };

      webserver = {
        rpc = mkOption {
          type = types.bool;
          default = true;
          description = "Enable RPC interface (-rpc). Required for zoekt-mcp and programmatic access.";
        };

        html = mkOption {
          type = types.bool;
          default = true;
          description = "Enable HTML web UI. Set false to run headless API-only.";
        };

        pprof = mkOption {
          type = types.bool;
          default = false;
          description = "Enable pprof profiling endpoint (-pprof). For debugging only.";
        };

        logDir = mkOption {
          type = types.str;
          default = logDir;
          description = "Directory for webserver log rotation (-log_dir).";
        };

        logRefresh = mkOption {
          type = types.str;
          default = "24h";
          description = "Log rotation interval (-log_refresh). Go duration format.";
        };
      };
    };

    # ── MCP options (from substrate hm-service-helpers) ───────────────
    mcp = mkMcpOptions {
      defaultPackage = pkgs.zoekt-mcp;
    };
  };

  # ── Config ─────────────────────────────────────────────────────────
  config = mkMerge [
    # Self-register with anvil unconditionally — enable flag controls activation.
    # Do NOT wrap in mkIf — it causes infinite recursion during module fixpoint.
    (mkAnvilRegistration {
      name = "zoekt";
      command = "zoekt-mcp";
      package = mcpCfg.package;
      enable = mcpCfg.enable;
      env.ZOEKT_URL = "http://localhost:${toString daemonCfg.port}";
      description = "Zoekt trigram code search";
      scopes = mcpCfg.scopes;
    })

    # Deprecated: serverEntry (kept for backward compatibility)
    (mkIf mcpCfg.enable {
      services.zoekt.mcp.serverEntry = mkMcpServerEntry {
        command = "${mcpCfg.package}/bin/zoekt-mcp";
        env.ZOEKT_URL = "http://localhost:${toString daemonCfg.port}";
      };
    })

    # Darwin: single launchd agent for zoekt-daemon
    (mkIf (daemonCfg.enable && isDarwin && (daemonCfg.repos != [] || githubCfg.enable)) (mkMerge [
      {
        home.activation.zoekt-index-dir = lib.hm.dag.entryAfter ["writeBoundary"] ''
          run mkdir -p "${daemonCfg.indexDir}"
          run mkdir -p "${webCfg.logDir}"
        '';
      }

      (mkLaunchdService {
        name = "zoekt-daemon";
        label = "io.pleme.zoekt-daemon";
        command = "${mcpCfg.package}/bin/zoekt-mcp";
        args = ["daemon" "--config" "${zoektDaemonConfig}"];
        logDir = "${config.home.homeDirectory}/Library/Logs";
      })
    ]))

    # Linux: single systemd service for zoekt-daemon
    (mkIf (daemonCfg.enable && !isDarwin && (daemonCfg.repos != [] || githubCfg.enable))
      (mkSystemdService {
        name = "zoekt-daemon";
        description = "Zoekt daemon — trigram-indexed code search";
        command = "${mcpCfg.package}/bin/zoekt-mcp";
        args = ["daemon" "--config" "${zoektDaemonConfig}"];
      }))
  ];
}
