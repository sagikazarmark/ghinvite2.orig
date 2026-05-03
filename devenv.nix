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
  };
}
