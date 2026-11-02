# Deprecated alias for the Flake in packaging/. It keeps existing
# `github:AprilNEA/OpenLogi` inputs working while users move to
# `github:AprilNEA/OpenLogi?dir=packaging`; remove it in a later release.
#
# The alias declares nixpkgs only so `inputs.openlogi.inputs.nixpkgs.follows`
# still reaches the package. It has no committed flake.lock (see .gitignore):
# packaging/flake.lock pins every other input. The relative path input needs
# Nix 2.26 or newer.
{
  description = "Deprecated alias for github:AprilNEA/OpenLogi?dir=packaging";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    openlogi = {
      url = "path:./packaging";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { openlogi, ... }:
    let
      moved = "the OpenLogi Flake moved: replace github:AprilNEA/OpenLogi with github:AprilNEA/OpenLogi?dir=packaging; the root alias will be removed in a later release";
    in
    builtins.warn moved openlogi.outputs;
}
