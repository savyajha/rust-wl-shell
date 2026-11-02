# The shell for home-manager: its config, generated from `settings` and checked (`--check`)
# when the configuration is built, and a user unit started once the graphical session is up.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.rust-wl-shell;
  kdl =
    value:
    if builtins.isBool value then
      lib.boolToString value
    else if builtins.isInt value then
      toString value
    else
      builtins.toJSON value;
  configFile =
    pkgs.runCommand "rust-wl-shell.kdl"
      {
        text = lib.concatStrings (lib.mapAttrsToList (key: value: "${key} ${kdl value}\n") cfg.settings);
        passAsFile = [ "text" ];
      }
      ''
        cp "$textPath" $out
        ${lib.getExe cfg.package} --check $out
      '';
in
{
  options.programs.rust-wl-shell = {
    enable = lib.mkEnableOption "rust-wl-shell, a macOS-style bar for niri";
    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./package.nix { };
      description = "The shell.";
    };
    settings = lib.mkOption {
      type =
        with lib.types;
        attrsOf (oneOf [
          bool
          int
          str
        ]);
      default = { };
      example = {
        height = 28;
        clock-format = "%A, %-d %b %H:%M";
        position = "bottom";
        secret-agent = false;
      };
      description = ''
        The config's keys and their values, as README.md lists them. Unknown keys and
        invalid values fail the build. `secret-agent = false` leaves NetworkManager's own
        requests for secrets to another agent (a panel that has one), while the network
        items stay. `niri-blur-rule = true` leaves the blur of the OSD to
        niri's `layer-rule`, which rounds its corners smoothly (README.md).
      '';
    };
    style = lib.mkOption {
      type = lib.types.lines;
      default = "";
      example = "window { --accent: #e01b24; --tint: rgba(0, 0, 0, 0.5); }";
      description = ''
        A stylesheet over the built-in one, written to
        `$XDG_CONFIG_HOME/rust-wl-shell/style.css`: overriding the built-in one's
        variables (README.md) is enough. A file GTK cannot parse leaves the built-in look.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];
    # The shell follows the file, so a switch restyles it without a restart.
    xdg.configFile."rust-wl-shell/style.css" = lib.mkIf (cfg.style != "") { text = cfg.style; };
    # After niri, as it is ready before the session's target is reached. Neither GDK_DEBUG
    # nor GSK_RENDERER is set here: the shell sets up its own GTK start-up (DESIGN.md).
    systemd.user.services.rust-wl-shell = {
      Unit = {
        Description = "macOS-style bar for niri";
        PartOf = [ "graphical-session.target" ];
        After = [ "graphical-session.target" ];
      };
      Service = {
        ExecStart = "${lib.getExe cfg.package} --config ${configFile}";
        Restart = "on-failure";
        RestartSec = 3;
      };
      Install.WantedBy = [ "graphical-session.target" ];
    };
  };
}
