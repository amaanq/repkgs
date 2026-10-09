# the devEnv of cpython.project, a python3 that also imports the checkout $MOTLEY_PROJECT names
{ pkgs }:
let
  project = pkgs.cpython.project { src = ./src; };
in
project.devEnv
