{ config, lib, pkgs, ... }:
let
  cfg = config.hardware.swipe-adjust;
  udevRulesPkg = pkgs.writeTextFile {
    name = "swipe-adjust-udev-rules";
    destination = "/etc/udev/rules.d/70-swipe-adjust.rules";
    text = ''
      SUBSYSTEM=="input", KERNEL=="event*", ATTRS{name}=="${cfg.tabletSwitchDeviceName}", TAG+="uaccess"
    '';
  };
in
{
  options.hardware.swipe-adjust = {
    enable = lib.mkEnableOption "udev seat ACLs for swipe-adjust tablet-mode switch detection";

    tabletSwitchDeviceName = lib.mkOption {
      type = lib.types.str;
      default = "gpio-keys";
      description = ''
        Kernel input device name (ATTRS{name}) exposing SW_TABLET_MODE to grant
        active seat read access via udev TAG+="uaccess".
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    # Must be ordered before systemd's 73-seat-late.rules (which runs RUN{builtin}+="uaccess");
    # services.udev.extraRules writes to 99-local.rules, which is too late for uaccess ACLs.
    services.udev.packages = [ udevRulesPkg ];

    # Re-trigger input event nodes on live `nixos-rebuild switch` so existing boot-time
    # devices receive the new uaccess seat ACL immediately without requiring a reboot.
    systemd.services.swipe-adjust-udev-trigger = {
      description = "Apply uaccess seat ACLs to swipe-adjust tablet-mode switch";
      wantedBy = [ "multi-user.target" ];
      after = [ "systemd-udevd.service" ];
      before = [ "display-manager.service" ];
      restartTriggers = [ udevRulesPkg ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = [
          "${pkgs.systemd}/bin/udevadm trigger --type=devices --action=change --subsystem-match=input --sysname-match=event*"
          "${pkgs.systemd}/bin/udevadm settle --timeout=5"
        ];
      };
    };

    # Ensure Intel pin control driver is available early so soc_button_array / gpio-keys
    # binds SW_TABLET_MODE reliably on Framework Laptop 12 boots.
    boot.initrd.kernelModules = lib.mkDefault [ "pinctrl_tigerlake" ];
  };
}
