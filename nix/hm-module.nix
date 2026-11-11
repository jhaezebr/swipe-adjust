{ self }:
{ config, lib, pkgs, ... }:
let
  cfg = config.services.swipe-adjust;
  tomlFormat = pkgs.formats.toml { };
in
{
  options.services.swipe-adjust = {
    enable = lib.mkEnableOption "touchscreen edge-swipe brightness and volume daemon";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "swipe-adjust.packages.\${pkgs.stdenv.hostPlatform.system}.default";
      description = "The swipe-adjust package to use.";
    };

    settings = lib.mkOption {
      type = tomlFormat.type;
      default = { };
      example = lib.literalExpression ''
        {
          output = "eDP-1";
          tablet_mode_only = true;
          edge_width_px = 22;
          vertical_margin_ratio = 0.12;
          sweep_height_ratio = 0.60;
          deadzone_px = 10.0;
          brightness = {
            device = "intel_backlight";
            min_percent = 1.0;
            gamma = 2.2;
          };
          volume = {
            max_percent = 100.0;
            auto_mute_at_zero = true;
            auto_unmute_on_up = true;
          };
          osd = {
            noctalia_sync = "auto";
          };
        }
      '';
      description = "Declarative TOML configuration written to ~/.config/swipe-adjust/config.toml.";
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    xdg.configFile."swipe-adjust/config.toml" = lib.mkIf (cfg.settings != { }) {
      source = tomlFormat.generate "swipe-adjust-config.toml" cfg.settings;
    };

    systemd.user.services.swipe-adjust = {
      Unit = {
        Description = "Touchscreen edge-swipe brightness and volume adjuster";
        PartOf = [ "graphical-session.target" ];
        After = [ "graphical-session.target" ];
        ConditionEnvironment = "WAYLAND_DISPLAY";
      };

      Service = {
        ExecStart = "${lib.getExe cfg.package} daemon";
        Restart = "on-failure";
        RestartSec = 2;
      };

      Install = {
        WantedBy = [ "graphical-session.target" ];
      };
    };
  };
}
