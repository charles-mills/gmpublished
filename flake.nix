{
  description = "Native desktop app for publishing Garry's Mod workshop addons.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    build-nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "git+https://github.com/oxalica/rust-overlay";
      inputs.nixpkgs.follows = "build-nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      build-nixpkgs,
      rust-overlay,
    }:
    let
      systems = [
        "x86_64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      eachSystem = build-nixpkgs.lib.genAttrs systems;

      pkgsFor =
        nixpkgsInput: system:
        import nixpkgsInput {
          inherit system;
          overlays = [ self.overlays.default ];
        };
    in
    {
      overlays.default = build-nixpkgs.lib.composeManyExtensions [
        rust-overlay.overlays.default
        (
          final: _prev:
          let
            rustToolchain = final.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
            rustPlatform = final.makeRustPlatform {
              cargo = rustToolchain;
              rustc = rustToolchain;
            };
          in
          {
            gmpublished = final.callPackage ./packaging/nix/package.nix { inherit rustPlatform; };
          }
        )
      ];

      packages = eachSystem (
        system:
        let
          pkgs = pkgsFor build-nixpkgs system;
        in
        {
          default = pkgs.gmpublished;
          gmpublished = pkgs.gmpublished;
        }
      );

      checks = eachSystem (
        system:
        let
          pkgs = pkgsFor build-nixpkgs system;
        in
        {
          gmpublished = pkgs.gmpublished;
        }
      );

      devShells = eachSystem (
        system:
        let
          pkgs = pkgsFor nixpkgs system;
          rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

          nativeLibs = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
            pkgs.alsa-lib
            pkgs.dbus
            pkgs.fontconfig
            pkgs.libGL
            pkgs.libx11
            pkgs.libxcb
            pkgs.libxcursor
            pkgs.libxi
            pkgs.libxkbcommon
            pkgs.vulkan-loader
            pkgs.wayland
          ];
        in
        {
          default = pkgs.mkShell {
            nativeBuildInputs = [
              rustToolchain
              pkgs.pkg-config
              pkgs.clang
              pkgs.lld
            ];

            buildInputs = nativeLibs;
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

            shellHook = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
              export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath nativeLibs}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
            '';
          };
        }
      );

      apps = eachSystem (system: {
        default = self.apps.${system}.gmpublished;
        gmpublished = {
          type = "app";
          program = "${self.packages.${system}.gmpublished}/bin/gmpublished";
          meta.description = "Run gmpublished";
        };
      });

      formatter = eachSystem (system: (pkgsFor nixpkgs system).nixfmt);
    };
}
