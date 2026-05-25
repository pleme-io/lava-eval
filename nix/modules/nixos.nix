# nix/modules/nixos.nix — auto-generated from lava-eval.caixa.lisp
# description: "In-memory tatara-lisp evaluator for lava architectures. Parses .tlisp source, evaluates (deflava-architecture ...) forms, produces typed lava_core::Architecture. Magma consumes this directly to do plan/apply in-memory — no on-disk JSON between authoring and apply. Extracted from lava-architectures for reuse by magma-lava + future consumers."
{ config, lib, pkgs, ... }:
let
  cfg = config.services.lava-eval;
in {
  options.services.lava-eval = {
    enable = lib.mkEnableOption "lava-eval";
    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.lava-eval or null;
    };
  };
  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
  };
}
