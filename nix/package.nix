{
  lib,
  rustPlatform,
  src,
}:
let
  # Read the version out of the workspace manifest so a release cannot drift
  # from the package metadata: the flake, the binary and the tag all come
  # from one number.
  workspacePackage = (lib.importTOML ../Cargo.toml).workspace.package;
in
rustPlatform.buildRustPackage (finalAttrs: {
  pname = "open-chat";
  version = workspacePackage.version;

  inherit src;

  # sha256 of the vendored crate graph, kept in a file so a Cargo.lock change
  # is a one-line diff. Refresh with scripts/refresh-cargo-hash.sh.
  cargoHash = lib.trim (builtins.readFile ./cargoHash.txt);

  # The suite is a real gate: it binds loopback ports against in-process mock
  # providers, so it needs no network and runs inside the sandbox.
  doCheck = true;

  strictDeps = true;

  # aws-lc-sys (via rustls) and the bundled SQLite build with the plain cc
  # builder on these targets, so no cmake or clang is pulled in. If a future
  # dependency needs them, add them here and say why.

  meta = {
    description = "Headless agent workspace engine: resource-governed Rust daemon with web, Telegram and OpenCode-gateway backends";
    homepage = "https://github.com/cmspeedau/open-chat";
    license = lib.licenses.mit;
    mainProgram = "openchatd";
    platforms = [
      "x86_64-linux"
      "aarch64-linux"
    ];
    sourceProvenance = [ lib.sourceTypes.fromSource ];
  };
})
