{
  platform,
  source,
  fetch,
  ...
}:
[
  {
    source = fetch.windowsSdk {
      arch = platform.cpu;
      manifest = source "default";
    };
  }
]
