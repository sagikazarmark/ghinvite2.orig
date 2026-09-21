{
  pkgs,
  lib,
  config,
  inputs,
  ...
}:

{
  dotenv.enable = true;

  dagger.enable = true;
  env.DAGGER_X_RELEASE = "v1.0.0-beta.14";

  packages = with pkgs; [
    lld
  ];

  languages = {
    rust = {
      enable = true;
    };
    javascript = {
      enable = true;
      package = pkgs.nodejs_22;
    };
  };
}
