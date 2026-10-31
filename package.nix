{
  lib,
  rustPlatform,
  pkg-config,
  wrapGAppsHook4,
  gsettings-desktop-schemas,
  gtk4,
  gtk4-layer-shell,
  libpulseaudio,
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
    # libpulse, for the volume (crates/core/src/audio.rs).
    libpulseaudio
  ];

  # The shell's own icons (crates/shell/src/main.rs).
  RUST_WL_SHELL_ICONS = "${placeholder "out"}/share/rust-wl-shell/icons";
  postInstall = ''
    install -Dm644 -t $out/share/rust-wl-shell/icons crates/shell/icons/*.svg
  '';

  # The keyboard layouts' short names (crates/core/src/xkb.rs).
  XKB_EVDEV_XML = "${xkeyboard_config}/share/X11/xkb/rules/evdev.xml";

  meta = {
    description = "A macOS-style top bar for niri, in GTK4";
    # crates/shell/src/adwaita.css, ported from libadwaita, is LGPL-2.1-or-later.
    license = [
      lib.licenses.mit
      lib.licenses.lgpl21Plus
    ];
    platforms = lib.platforms.linux;
    mainProgram = "rust-wl-shell";
  };
}
