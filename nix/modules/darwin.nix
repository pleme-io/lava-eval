# nix/modules/darwin.nix — auto-generated from lava-eval.caixa.lisp
{ config, lib, pkgs, ... }:
let cfg = config.services.lava-eval; in {
  options.services.lava-eval = {
    enable = lib.mkEnableOption "lava-eval";
    package = lib.mkOption { type = lib.types.package; default = pkgs.lava-eval or null; };
  };
  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
  };
}
