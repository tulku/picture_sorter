{
  description = "Offline photo archive manager";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      eachSystem = nixpkgs.lib.genAttrs systems;
    in {
      packages = eachSystem (system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "photo-sorter";
            version = "0.1.0";
            src = pkgs.lib.cleanSource self;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [ "--bins" ];
            cargoTestFlags = [ "--all-targets" ];
            nativeBuildInputs = [ pkgs.makeWrapper pkgs.pkg-config ];
            buildInputs = [ pkgs.sqlite ];
            nativeCheckInputs = [ pkgs.exiftool pkgs.rsync pkgs.bash pkgs.util-linux ];
            postPatch = ''
              substituteInPlace tests/packaging.rs \
                --replace-fail '#!/usr/bin/env bash' '#!${pkgs.bash}/bin/bash'
            '';
            postInstall = ''
              for binary in photo_sorter photo_sorter_legacy; do
                wrapProgram "$out/bin/$binary" \
                  --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.exiftool pkgs.rsync ]}
              done
            '';
            meta = {
              description = "Offline photo import, working copies and verified archive publication";
              platforms = systems;
              mainProgram = "photo_sorter";
            };
          };
        });
      devShells = eachSystem (system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in {
          default = pkgs.mkShell {
            packages = with pkgs; [ cargo rustc rustfmt clippy gcc exiftool rsync ];
          };
        });
    };
}
