{
  lib,
  rustPlatform,
  pkg-config,
  wrapGAppsHook4,
  gsettings-desktop-schemas,
  gtk4,
  gtk4-layer-shell,
  xkeyboard_config,
}:

rustPlatform.buildRustPackage {
  pname = "rust-wl-shell";
  version = "0.1.0";

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.toml
      ./Cargo.lock
      ./crates
    ];
  };

  cargoLock.lockFile = ./Cargo.lock;
  cargoBuildFlags = [
    "--package"
    "rust-wl-shell"
  ];
  cargoTestFlags = [ "--workspace" ];

  # The wrapper gives GTK its GSettings schemas and the dconf backend: without the portal
  # (DESIGN.md, "Start-up"), GTK reads the colour scheme and font from there.
  nativeBuildInputs = [
    pkg-config
    wrapGAppsHook4
  ];
  buildInputs = [
    gsettings-desktop-schemas
    gtk4
    gtk4-layer-shell
  ];

  # The keyboard layouts' short names (crates/core/src/xkb.rs).
  XKB_EVDEV_XML = "${xkeyboard_config}/share/X11/xkb/rules/evdev.xml";

  meta = {
    description = "A macOS-style top bar for niri, in GTK4";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = "rust-wl-shell";
  };
}
