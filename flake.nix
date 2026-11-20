{
  description = "open-chat: headless agent workspace engine (resource-governed Rust daemon)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      lib = nixpkgs.lib;

      # Only the platforms a runner actually proves. aarch64-linux was here
      # too and is dropped for now rather than left as an unverified claim:
      # the flake builds it in principle, but until an arm runner is green we
      # do not list it as supported.
      systems = [ "x86_64-linux" ];

      eachSystem = f: lib.genAttrs systems (system: f system);

      pkgsFor = eachSystem (system: import nixpkgs { inherit system; });

      # What the crate needs to build: manifests, sources (the web assets are
      # compiled in with include_str!), and the licence text.
      srcFor = eachSystem (
        system:
        lib.fileset.toSource {
          root = ./.;
          fileset = lib.fileset.unions [
            ./Cargo.toml
            ./Cargo.lock
            ./LICENSE
            ./README.md
            ./crates
            ./packaging
            ./nix
          ];
        }
      );

      # Single source of truth for the version: the workspace manifest.
      manifestVersion = (lib.importTOML ./Cargo.toml).workspace.package.version;
    in
    {
      packages = eachSystem (
        system:
        let
          openchatd = pkgsFor.${system}.callPackage ./nix/package.nix {
            src = srcFor.${system};
          };
        in
        {
          default = openchatd;
          inherit openchatd;
        }
      );

      # `nix flake check` builds the package (including its test suite) and
      # runs these extra gates.
      checks = eachSystem (
        system:
        let
          pkg = self.packages.${system}.openchatd;
        in
        {
          # Catches manifest/lock/filename drift before a tag does: the bytes
          # we ship must report the version the manifest claims.
          version-is-manifest = pkgsFor.${system}.runCommand "open-chat-version-gate" { } ''
            reported="$(${pkg}/bin/openchatd --version)"
            if [ "$reported" != "openchatd ${manifestVersion}" ]; then
              echo "version gate failed: binary says '$reported', manifest says '${manifestVersion}'" >&2
              exit 1
            fi
            touch "$out"
          '';
        }
      );

      devShells = eachSystem (
        system:
        let
          pkgs = pkgsFor.${system};
        in
        {
          default = pkgs.mkShell {
            description = "open-chat development shell (rust + the packaging tools the scripts use)";

            packages = [
              pkgs.cargo
              pkgs.clippy
              pkgs.rustc
              pkgs.rustfmt
              pkgs.rust-analyzer

              # packaging/build-appimage.sh and scripts/ requirements
              pkgs.binutils # objdump: glibc floor gate
              pkgs.curl
              pkgs.desktop-file-utils
              pkgs.git
              pkgs.python3
              pkgs.squashfsTools # mksquashfs + unsquashfs
              pkgs.stdenv.cc # cc builder for aws-lc-sys / bundled SQLite
              pkgs.zsync # zsyncmake for delta updates
            ]
            ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.appimage-run ];

            shellHook = ''
              echo "open-chat dev shell: cargo test --workspace, packaging/build-appimage.sh"
            '';
          };
        }
      );

      formatter = eachSystem (system: pkgsFor.${system}.nixfmt);
    };
}
