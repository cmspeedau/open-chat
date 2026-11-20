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

  # buildRustPackage's default check phase execs each compiled test binary
  # and passes checkFlags to libtest, so cargo-level scoping does not belong
  # there (CI proved it: `error: Unrecognized option: 'workspace'`). Run the
  # suite through cargo explicitly instead, excluding only the watcher crate:
  # its tests wait for real inotify events and the Nix sandbox delivers none,
  # so three of them time out while everything else is green. Excluding that
  # crate is honest scope - the alternative is reporting filesystem watching
  # as proven where nothing could be observed. The cargo CI job runs all 204
  # tests on a normal kernel and remains the gate for the watcher.
  checkPhase = ''
    runHook preCheck
    cargo test --offline --locked --release --workspace --exclude openchat-watcher
    runHook postCheck
  '';

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
