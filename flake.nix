{
  description = "rust devshell";
  nixConfig.bash-prompt = "[nix develop] ";
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    nixpkgs-intel-darwin.url = "github:NixOS/nixpkgs/nixpkgs-26.05-darwin";

  };

  outputs =
    { nixpkgs, nixpkgs-intel-darwin, ... }:
    let
      darwinShell = system:
        let
          pkgs = import (if system == "x86_64-darwin" then nixpkgs-intel-darwin else nixpkgs) {
            inherit system;
            config.allowUnfree = true;
          };
        in
        pkgs.mkShell {
          buildInputs = with pkgs; [
            rustc
            cargo
            gh
            python3Packages.cryptography
            rustfmt
            whisper-cpp
          ];

          # cidre builds and links Objective-C shims against Apple frameworks.
          # Use Xcode's toolchain rather than Nix's clang wrapper so it matches
          # the system linker and SDK selected by xcodebuild.
          shellHook = ''
            export CC=/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin/clang
            export CXX=/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin/clang++
            export SDKROOT=/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk
            export RUSTFLAGS="-C linker=$CC -C link-arg=-mmacosx-version-min=14.4"
            export MACOSX_DEPLOYMENT_TARGET=14.4
          '';
        };
    in
    {
      devShell.aarch64-darwin = darwinShell "aarch64-darwin";

      devShell.x86_64-darwin = darwinShell "x86_64-darwin";

      devShell.x86_64-linux =
        let
          pkgs = import nixpkgs {
            system = "x86_64-linux";
          };
        in
        pkgs.mkShell {
          buildInputs = with pkgs; [
            alsa-lib
            binutils
            cargo
            gh
            pkg-config
            pulseaudio
            rustc
            rustfmt
          ];
        };
    };
}
