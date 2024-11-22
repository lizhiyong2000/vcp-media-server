#! /bin/zsh -
for toml (**/Cargo.toml(N.)) cargo -v "$@" --manifest-path $toml