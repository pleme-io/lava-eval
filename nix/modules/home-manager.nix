# nix/modules/home-manager.nix — auto-generated from lava-eval.caixa.lisp
{ config, lib, pkgs, ... }:
let cfg = config.programs.lava-eval; in {
  options.programs.lava-eval = {
    enable = lib.mkEnableOption "lava-eval";
    package = lib.mkOption { type = lib.types.package; default = pkgs.lava-eval or null; };
  };
  config = lib.mkIf cfg.enable { home.packages = [ cfg.package ]; };
}
