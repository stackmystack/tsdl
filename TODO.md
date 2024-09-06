# TODO

## Commands

- [ ] check command: check that all tools necessary are installed (gunzip, wget, curl, git)

## Configurtion

- [ ] Investigate a figment replacement / custom impl to merge diferent configuration
    sources.

### Options

- [ ] --postfix
- [ ] --sys-ts, false by default
  - [x] Add the flag.
  - [ ] Use [TREE_SITTER_LIBDIR](https://github.com/tree-sitter/tree-sitter/blob/4f97cf850535a7b23e648aba6e355caed1f10231/cli/loader/src/lib.rs#L177)
        by default
  - [ ] Use pkgconfig for sys libs

## Tests

- [ ] changing log file destination from command line, apparently it's not working.
