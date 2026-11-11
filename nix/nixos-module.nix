{ config, lib, ... }:
let
  cfg = config.hardware.swipe-adjust;
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
    # Grant the active local seat user read-only access to the tablet-mode switch
    # without adding the user to the privileged global 'input' group.
    services.udev.extraRules = ''
      SUBSYSTEM=="input", KERNEL=="event*", ATTRS{name}=="${cfg.tabletSwitchDeviceName}", TAG+="uaccess"
    '';

    # Ensure Intel pin control driver is available early so soc_button_array / gpio-keys
    # binds SW_TABLET_MODE reliably on Framework Laptop 12 boots.
    boot.initrd.kernelModules = lib.mkDefault [ "pinctrl_tigerlake" ];
  };
}
