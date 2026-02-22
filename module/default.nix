# Zoekt home-manager module — daemon (webserver + indexer) + MCP server entry
#
# Namespace: services.zoekt.daemon.* / services.zoekt.mcp.*
#
# The daemon manages zoekt-webserver (persistent) + zoekt-git-index (periodic).
# The MCP entry exposes a serverEntry attrset for consumption by claude modules.
#
# Zoekt itself comes from nixpkgs (Go package). zoekt-mcp (this flake's package)
# is the Rust MCP wrapper that talks to the webserver's HTTP/RPC API.
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
  inherit (hmHelpers)
    mkMcpOptions mkMcpServerEntry
    mkLaunchdService mkLaunchdPeriodicTask
    mkSystemdService mkSystemdPeriodicTask;
  daemonCfg = config.services.zoekt.daemon;
  mcpCfg = config.services.zoekt.mcp;
  ctagsCfg = daemonCfg.ctags;
  webCfg = daemonCfg.webserver;
  isDarwin = pkgs.stdenv.isDarwin;

  # ── Zoekt indexer wrapper (ensures ctags is on PATH) ─────────────────
  zoektIndexerScript = let
    ctagsArgs =
      if ctagsCfg.enable
      then (optionals ctagsCfg.require ["-require_ctags"])
      else ["-disable_ctags"];
    deltaArgs = optionals daemonCfg.delta ["-delta"];
    branchArgs = optionals (daemonCfg.branches != "HEAD") ["-branches" daemonCfg.branches];
    largeFileArgs = concatMap (p: ["-large_file" p]) daemonCfg.largeFiles;
    parallelismArgs = ["-parallelism" (toString daemonCfg.parallelism)];
    fileLimitArgs = ["-file_limit" (toString daemonCfg.fileLimit)];
    allArgs = ctagsArgs ++ deltaArgs ++ branchArgs ++ largeFileArgs ++ parallelismArgs ++ fileLimitArgs;
    repoArgs = concatStringsSep " " (map (r: ''"${r}"'') daemonCfg.repos);
    ctagsPath =
      if ctagsCfg.enable
      then "${ctagsCfg.package}/bin"
      else "";
  in
    pkgs.writeShellScript "zoekt-indexer" ''
      ${optionalString isDarwin ''
      logDir="${config.home.homeDirectory}/Library/Logs"
      : > "$logDir/zoekt-indexer.log"
      : > "$logDir/zoekt-indexer.err"
      ''}
      export PATH="${ctagsPath}:${daemonCfg.package}/bin:${pkgs.git}/bin:$PATH"
      exec zoekt-git-index \
        -index "${daemonCfg.indexDir}" \
        ${concatStringsSep " " allArgs} \
        ${repoArgs}
    '';

  # Webserver args (shared between Darwin and Linux)
  webserverArgs = [
    "-index" daemonCfg.indexDir
    "-listen" ":${toString daemonCfg.port}"
    "-log_dir" webCfg.logDir
    "-log_refresh" webCfg.logRefresh
  ]
  ++ optionals webCfg.rpc ["-rpc"]
  ++ optionals webCfg.pprof ["-pprof"]
  ++ optionals (!webCfg.html) ["-html=false"];

  logDir = if isDarwin
    then "${config.home.homeDirectory}/Library/Logs"
    else "${config.home.homeDirectory}/.local/share/zoekt/logs";
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
        description = "Re-index interval in seconds (launchd StartInterval / systemd timer OnUnitActiveSec)";
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
    # MCP server entry
    (mkIf mcpCfg.enable {
      services.zoekt.mcp.serverEntry = mkMcpServerEntry {
        command = "${mcpCfg.package}/bin/zoekt-mcp";
        env.ZOEKT_URL = "http://localhost:${toString daemonCfg.port}";
      };
    })

    # Darwin: launchd agents for zoekt-webserver + zoekt-indexer
    (mkIf (daemonCfg.enable && isDarwin && daemonCfg.repos != []) (mkMerge [
      {
        home.activation.zoekt-index-dir = lib.hm.dag.entryAfter ["writeBoundary"] ''
          run mkdir -p "${daemonCfg.indexDir}"
          run mkdir -p "${webCfg.logDir}"
        '';
      }

      (mkLaunchdService {
        name = "zoekt-webserver";
        label = "io.pleme.zoekt-webserver";
        command = "${daemonCfg.package}/bin/zoekt-webserver";
        args = webserverArgs;
        logDir = "${config.home.homeDirectory}/Library/Logs";
      })

      (mkLaunchdPeriodicTask {
        name = "zoekt-indexer";
        label = "io.pleme.zoekt-indexer";
        command = "${zoektIndexerScript}";
        interval = daemonCfg.indexInterval;
        logDir = "${config.home.homeDirectory}/Library/Logs";
      })
    ]))

    # Linux: systemd user services for zoekt-webserver + zoekt-indexer
    (mkIf (daemonCfg.enable && !isDarwin && daemonCfg.repos != []) (mkMerge [
      (mkSystemdService {
        name = "zoekt-webserver";
        description = "Zoekt webserver — trigram-indexed code search";
        command = "${daemonCfg.package}/bin/zoekt-webserver";
        args = webserverArgs;
        preStart = "${pkgs.coreutils}/bin/mkdir -p ${daemonCfg.indexDir} ${webCfg.logDir}";
      })

      (mkSystemdPeriodicTask {
        name = "zoekt-indexer";
        description = "Zoekt periodic indexer";
        command = "${zoektIndexerScript}";
        interval = daemonCfg.indexInterval;
        after = ["zoekt-webserver.service"];
      })
    ]))
  ];
}
