{
  lib,
  rustPlatform,
  cacert,
  makeWrapper,
  rustfmt,
  sqlite,
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

  # The suite binds loopback ports against in-process mock providers, so it
  # needs no network - but CI showed it does need a CA store, because reqwest
  # builds a root store whenever a Client is constructed. See cacert below.
  doCheck = true;

  # The watcher crate's tests wait for real inotify events. The Nix sandbox
  # delivers none, so three of them time out there (CI log: "timed out
  # waiting for event: Elapsed" on removal_is_reported,
  # ignores_do_not_wake_subscribers and coalesces_bursts_into_one_event_per_path),
  # while every other test passes. Excluding that crate here is the honest
  # scope - the alternative would be asserting filesystem watching works by
  # never observing it. The cargo CI job runs the same suite on a normal
  # kernel and remains the gate for the watcher.
  checkFlags = [
    "--workspace"
    "--exclude"
    "openchat-watcher"
  ];

  strictDeps = true;

  # The link line asks for -lsqlite3 (from the sqlite-vec build script) even
  # though rusqlite compiles SQLite in statically. On a normal distro that
  # resolves against the distro's libsqlite3.so at link time and then
  # `--as-needed` drops the dependency, so the binary has no runtime
  # libsqlite3 requirement - verified here with ldd. The Nix sandbox has no
  # implicit libsqlite3, so provide one purely to satisfy the linker.
  buildInputs = [ sqlite ];

  # reqwest 0.13's rustls platform verifier builds its root store from the
  # system trust store when a Client is *constructed*, and it fails the build
  # if no CA bundle is found - even when the client only ever talks to
  # http://127.0.0.1. The sandbox has no /etc/ssl/certs, so point it at the
  # nixpkgs bundle, which is what any ordinary distro supplies implicitly.
  # Verified from the CI log: 11 daemon tests failed with exactly
  # "No CA certificates were loaded from the system", all of them only because
  # they build a client.
  nativeBuildInputs = [
    cacert
    makeWrapper
    # The patch engine formats Rust by running `rustfmt`, and two tests
    # assert that it happened. rustPlatform provides cargo and rustc but not
    # the formatter, so without this the suite fails for a missing tool
    # rather than for a broken engine.
    rustfmt
  ];

  SSL_CERT_FILE = "${cacert}/etc/ssl/certs/ca-bundle.crt";
  NIX_SSL_CERT_FILE = "${cacert}/etc/ssl/certs/ca-bundle.crt";
  # ...and the openssl-style directory, for anything that scans a hashed
  # cert store instead of reading a single bundle file.
  SSL_CERT_DIR = "${cacert}/etc/ssl/certs";

  # The same reasoning applies after installation: a daemon started from the
  # store should not depend on the host happening to expose a discoverable CA
  # bundle, or the first client it constructs fails at build time. Wrapping
  # pins the bundle the build was given. checks.version-is-manifest runs
  # $out/bin/openchatd, so a broken wrapper cannot pass silently.
  postInstall = ''
    wrapProgram "$out/bin/openchatd" --set SSL_CERT_FILE "${cacert}/etc/ssl/certs/ca-bundle.crt"
  '';

  # nixpkgs' cargo environment asks aws-lc-sys (through rustls) to link a
  # system-installed AWS-LC; nothing here provides one, so the build script
  # panics. Building the vendored source is not just a workaround - it is what
  # every other build of this project does (local cargo, the CI cargo job, the
  # AppImage), so the flake must match it rather than ship a different crypto
  # provider behind the same version number. If a future nixpkgs gives us a
  # properly patched system aws-lc and we decide to link it, that is a crypto
  # supply-chain decision to make explicitly, not one a setup hook makes for
  # us. On these targets aws-lc-sys and the bundled SQLite build with the
  # plain cc builder, so no cmake or clang is pulled in; if a future
  # dependency needs them, add them here and say why.
  preConfigure = ''
    unset AWS_LC_SYS_USE_SYSTEM
  '';
  preBuild = ''
    unset AWS_LC_SYS_USE_SYSTEM
  '';
  preCheck = ''
    unset AWS_LC_SYS_USE_SYSTEM
  '';

  # aws-lc-sys (via rustls) and the bundled SQLite build with the plain cc
  # builder on these targets, so no cmake or clang is pulled in. If a future
  # dependency needs them, add them here and say why.

  meta = {
    description = "Headless agent workspace engine: resource-governed Rust daemon with web, Telegram and OpenCode-gateway backends";
    homepage = "https://github.com/cmspeedau/open-chat";
    license = lib.licenses.mit;
    mainProgram = "openchatd";
    platforms = [ "x86_64-linux" ];
    sourceProvenance = [ lib.sourceTypes.fromSource ];
  };
})
