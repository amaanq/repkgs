# Every custom command runs through the launcher bigtop's cmake system passes as
# -DPKGS_CUSTOM_LAUNCHER, mummer's tblgenwrap, which caches tablegen runs.
if(PKGS_CUSTOM_LAUNCHER)
  set_property(GLOBAL PROPERTY RULE_LAUNCH_CUSTOM "${PKGS_CUSTOM_LAUNCHER}")
endif()
