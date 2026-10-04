{
  pkgs,
  source,
  fetch,
  ...
}:
[
  {
    pyapp.deps = fetch.pythonDeps {
      python = pkgs.cpython;
      sdist = [ "pydantic-core" ];
      source = source "default";
    };
  }
]
