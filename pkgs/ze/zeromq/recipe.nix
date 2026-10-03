{ platform, when, ... }:
[
  # the select poller cannot carry IPC, which windows has over AF_UNIX, so use the bundled wepoll
  (when (platform.os == "windows") { cmake.defs.POLLER = "epoll"; })
]
