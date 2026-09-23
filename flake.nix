{
  description = "Darask Paint for NixOS (Wayland and X11)";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      config = system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          lib = pkgs.lib;
          libraries = with pkgs; [ libGL wayland libxkbcommon libX11 libXcursor libXi libXrandr libxcb dbus ];
          tools = with pkgs; [ libarchive xterm bash fontconfig zenity ];
          font = "${pkgs.ipaexfont}/share/fonts/truetype/ipaexg.ttf";
          package = pkgs.rustPlatform.buildRustPackage {
            pname = "darask-paint";
            version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [ ./Cargo.toml ./Cargo.lock ./build.rs ./src ./assets ./examples ];
            };
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = with pkgs; [ pkg-config makeWrapper ];
            buildInputs = libraries;
            nativeCheckInputs = tools;
            DARASK_FONT_FILE = font;
            postInstall = ''
              wrapProgram $out/bin/darask-paint \
                --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath libraries} \
                --prefix PATH : ${lib.makeBinPath tools} \
                --set-default DARASK_FONT_FILE ${font}
              install -Dm644 assets/darask-paint.desktop $out/share/applications/darask-paint.desktop
              install -Dm644 assets/icon-256.png $out/share/icons/hicolor/256x256/apps/darask-paint.png
            '';
            meta = {
              description = "Lightweight Japanese raster image editor";
              homepage = "https://github.com/daraskme/darask-paint";
              mainProgram = "darask-paint";
              platforms = systems;
            };
          };
        in {
          inherit package;
          shell = pkgs.mkShell {
            inputsFrom = [ package ];
            packages = tools ++ (with pkgs; [ cargo rustc rustfmt clippy ]);
            LD_LIBRARY_PATH = lib.makeLibraryPath libraries;
            DARASK_FONT_FILE = font;
          };
        };
    in {
      packages = forAllSystems (system: { default = (config system).package; });
      apps = forAllSystems (system: {
        default = { type = "app"; program = "${self.packages.${system}.default}/bin/darask-paint"; };
      });
      devShells = forAllSystems (system: { default = (config system).shell; });
      checks = forAllSystems (system: { package = self.packages.${system}.default; });
    };
}
