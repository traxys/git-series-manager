self: {
  lib,
  pkgs,
  config,
  ...
}:
with lib; let
  cfg = config.programs.git-series-manager;

  tomlFormat = pkgs.formats.toml {};
in {
  options.programs.git-series-manager = {
    enable = mkEnableOption "git-series-manager, a way to manage git patchsets";

    package = mkOption {
      type = types.package;
      description = "Package to use by git-series-manager";
      default = self.defaultPackage."${pkgs.system}";
    };

    settings = mkOption {
      inherit (tomlFormat) type;
      example = lib.literalExpression ''
        {
          sendmail_args = ["--sendmail-cmd=customSendmail" "--to=mail@list.com"];
          repo_url_base = "https://my.git-forge.com/my/project/";
          ci_url = "https://my.jenkins.instance/''${component}/job/''${branch}/''${ci_job}";
          editor = "nvim";
        }
      '';
      description = ''
        git-series-manager global configuration, can also be configured through ''${repo}/.patches/config.toml
      '';
    };
  };

  config = mkIf cfg.enable {
    home.packages = [ cfg.package ];

    xdg.configFile."git-series-manager/config.toml".source = 
    tomlFormat.generate "config.toml" cfg.settings;
  };
}
