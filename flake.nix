{
  description = "rust devshell";
  nixConfig.bash-prompt = "[nix develop] ";
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";

  };

  outputs =
    { nixpkgs, ... }:
    {
      devShell.aarch64-darwin =
        let
          pkgs = import nixpkgs {
            system = "aarch64-darwin";
            config.allowUnfree = true;
          };
        in
        pkgs.mkShell {
          buildInputs = with pkgs; [
            rustc
            cargo
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
    };
}
