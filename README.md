# `swipe-adjust`

Lightweight, event-driven Wayland daemon for convertible 2-in-1 touchscreens (built for **Framework Laptop 12** + **Niri** + **Noctalia** on **NixOS**):

- **Left bezel swipe (up/down):** Smooth continuous 1:1 display brightness scrubbing (`systemd-logind` / `sysfs`, perceptual $\gamma = 2.2$ curve, safe 1% floor).
- **Right bezel swipe (up/down):** Smooth continuous 1:1 speaker volume scrubbing (native PipeWire/PulseAudio socket, 100% cap, auto-mute at 0% & auto-unmute on upward swipe).
- **Tablet-mode auto-activation:** Listens to hardware `SW_TABLET_MODE` (`gpio-keys`) and completely unmaps the Wayland edge strips in laptop mode so right-edge mouse scrollbars are never obstructed.
- **Zero touch leakage & automatic rotation:** Uses `zwlr_layer_shell_v1` + `wp_single_pixel_buffer_v1` `wl_touch` strips, preventing background windows from scrolling while swiping and rotating automatically alongside `iio-niri`.
- **Ultra power-efficient:** Single-threaded `poll()` loop — **0.00% CPU and 0 wakeups/sec** when idle, zero subprocess forks (`wpctl`/`brightnessctl` are never spawned), ~2–4 MB RAM.

## CLI Usage

```bash
swipe-adjust daemon [--config ~/.config/swipe-adjust/config.toml]
swipe-adjust status     # Print JSON state (active, mode, tabletModeHw, output)
swipe-adjust toggle     # Manually toggle edge strips on/off
swipe-adjust enable     # Force-enable edge strips (even in laptop mode)
swipe-adjust disable    # Force-disable edge strips
swipe-adjust auto       # Return to automatic SW_TABLET_MODE tracking
```

## NixOS + Home Manager Flake Integration

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    home-manager.url = "github:nix-community/home-manager";
    swipe-adjust = {
      url = "path:/home/jhaezebr/Projects/VibeCoded/swipe-adjust"; # or github:<user>/swipe-adjust
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { nixpkgs, home-manager, swipe-adjust, ... }: {
    nixosConfigurations.framework = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ./configuration.nix

        # 1. System module: adds udev TAG+="uaccess" on gpio-keys for SW_TABLET_MODE
        swipe-adjust.nixosModules.default
        {
          hardware.swipe-adjust.enable = true;
        }

        # 2. Home Manager module: configures TOML & graphical-session user service
        home-manager.nixosModules.home-manager
        {
          home-manager.users.jhaezebr = {
            imports = [ swipe-adjust.homeManagerModules.default ];
            services.swipe-adjust = {
              enable = true;
              settings = {
                output = "eDP-1";
                tablet_mode_only = true;
                edge_width_px = 22;
                vertical_margin_ratio = 0.12;
                sweep_height_ratio = 0.60;
                deadzone_px = 10.0;
              };
            };
          };
        }
      ];
    };
  };
}
```
