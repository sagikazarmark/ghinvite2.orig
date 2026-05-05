{
  pkgs,
  lib,
  config,
  inputs,
  ...
}:

{
  dotenv.enable = true;
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
