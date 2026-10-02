# Other hosts serving the same files, keyed by the URL prefix they replace. A recipe names the
# canonical URL, and nix/sources.nix passes the fetcher that URL plus each rewrite as `urls`.
{
  "https://ftp.gnu.org/gnu/" = [
    "https://ftpmirror.gnu.org/"
    "https://mirrors.kernel.org/gnu/"
  ];
  "https://ftpmirror.gnu.org/" = [
    "https://ftp.gnu.org/gnu/"
    "https://mirrors.kernel.org/gnu/"
  ];
  "https://download.savannah.gnu.org/releases/" = [
    "https://download-mirror.savannah.gnu.org/releases/"
  ];
  "https://download.savannah.nongnu.org/releases/" = [
    "https://download-mirror.savannah.gnu.org/releases/"
  ];
  "https://www.kernel.org/pub/" = [ "https://mirrors.edge.kernel.org/pub/" ];
  "https://cdn.kernel.org/pub/" = [ "https://mirrors.edge.kernel.org/pub/" ];
  "https://sourceware.org/pub/" = [ "https://mirrors.kernel.org/sourceware/" ];
}
