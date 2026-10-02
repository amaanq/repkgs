{
  source,
  file,
  fetch,
  ...
}:
[
  {
    npm.deps = fetch.npmDeps {
      lockFile = file "package-lock.json";
      source = source "default";
    };
  }
]
