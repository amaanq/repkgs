{
  pkgs,
  pin,
  source,
  fetch,
  ...
}:
[
  {
    env.SETUPTOOLS_SCM_PRETEND_VERSION = pin.version;
    pyapp.deps = fetch.pythonDeps {
      python = pkgs.cpython;
      sdist = [ "pydantic-core" ];
      source = source "default";
    };
  }
]
