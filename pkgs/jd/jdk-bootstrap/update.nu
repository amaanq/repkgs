# Temurin spells the tag jdk-25.0.4.1+1 as 25.0.4.1_1 in file names and with %2B in URL paths
export def resolve [pkg: record]: nothing -> table {
   ^spieler versions $pkg.upstream.purl | from json | each {|release|
      let version = ($release.tag | str replace "jdk-" "")
      $release | merge {version: $version, path: ($release.tag | str replace "+" "%2B"), file: ($version | str replace -ar "[.+]" "_")}
   }
}
