# TODO

## CI

- [ ] Cross-platform builds: support all tree-sitter platforms.
  - [ ] Package
    - [ ] Linux
    - [ ] Mac
    - [ ] Tar / Zip
  - [ ] Release binaries and separate linux/mac distribution packages.
    - [ ] Github
    - [ ] crates.io

## Commands

- [ ] check command: check that all tools necessary are installed (gunzip, wget, curl, git)

## Configurtion

- [ ] Investigate a figment replacement / custom impl to merge diferent configuration
    sources.

## Maintenance

- [ ] A sane way to produce change logs
- [ ] just release {{arg}}
  - [ ] {{arg}} is a version number => tag with v{{args}}
  - [ ] Handle changelog
  - [ ] push to main repo with tags
  - [ ] CI should kick in

### Options

- [ ] --sys-ts, false by default
  - [x] Add the flag.
  - [ ] Use [TREE_SITTER_LIBDIR](https://github.com/tree-sitter/tree-sitter/blob/4f97cf850535a7b23e648aba6e355caed1f10231/cli/loader/src/lib.rs#L177)
        by default
  - [ ] Use pkgconfig for sys libs

## Tests

- [ ] Use assert_cmd
  - [ ] Test --force
  - [ ] Test --sys-ts
- [ ] Config
  - [ ] with default config
    - [ ] You can always download a parser even if it's not in the config.
    - [ ] Verify it's actually HEAD when the parser is not in the config using git in the test.
  - [ ] with custom config file.
    - [ ] ask for parsers defined in the config file
    - [ ] ask for parsers !defined in the config file and verify they're from the repo's HEAD.
