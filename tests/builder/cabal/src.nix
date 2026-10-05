{
  "Main.hs" = ''
    module Main where

    main :: IO ()
    main = putStrLn "test-cabal 0.1.0.0"
  '';
  "test-cabal.cabal" = ''
    cabal-version: 3.0
    name: test-cabal
    version: 0.1.0.0
    build-type: Simple

    executable test-cabal
      main-is: Main.hs
      build-depends: base
      default-language: Haskell2010
  '';
}
