{
  pkgs,
  lib,
  config,
  inputs,
  ...
}:

{
  packages = with pkgs; [
    lld
  ];

  languages = {
    rust = {
      enable = true;
    };
    javascript = {
      enable = true;
      package = pkgs.nodejs_20;
    };
  };
}
