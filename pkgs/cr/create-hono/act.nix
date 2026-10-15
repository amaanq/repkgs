{
  source,
  file,
  fetch,
  ...
}:
[
  {
    test.registry = fetch.npmDeps {
      lockFile = file "starter-lock.json";
      source = source "default";
    };
  }
]
