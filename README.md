# TSDL

[![CI badge]][CI]
[![crates.io badge]][crates.io]

[CI]: https://github.com/stackmystack/tsdl/actions/workflows/ci.yml
[CI badge]: https://github.com/stackmystack/tsdl/actions/workflows/ci.yml/badge.svg
[crates.io]: https://crates.io/crates/tsdl
[crates.io badge]: https://img.shields.io/crates/v/tsdl.svg

A downloader/builder of many
[tree-sitter](https://github.com/tree-sitter/tree-sitter) parsers

## Why?

To build parsers (`.so`/`.dylib`) and use them with your favourite bindings.

I created it more specifically for the [ruby bindings](https://github.com/Faveod/ruby-tree-sitter).

## Installation

You can either grab the binary for your platform from
[`tsdl`'s releases](https://github.com/stackmystack/tsdl/releases/latest)
or install via cargo:

```sh
cargo install tsdl
```

## Usage

To build a parser:

```sh
tsdl build rust
```

To build many parsers:

```sh
tsdl build rust ruby json
```

If a configuration file (`parsers.toml`) is provided, then simply running:

```sh
tsdl build
```

will download all the pinned parsers.

## Configuration

If no configuration is provided for the language you're asking for in `parsers.toml`,
the latest parsers will be downloaded built.

If you wish to pin parser versions:

```toml
[parsers]
java = "v0.21.0"
json = "0.21.0" # The leading v is not necessary
python = "master"
typescript = { ref = "0.21.0", cmd = "make" }
cobol = { ref = "6a469068cacb5e3955bb16ad8dfff0dd792883c9", from = "https://github.com/yutaro-sakamoto/tree-sitter-cobol" }
```

Run:

```sh
tsdl config default
```

to get the default config used by tsdl in TOML.

> [!IMPORTANT]
> All configuration you can pass to `tsd build` can be put in the `parsers.toml`,
> like `tree-sitter-version`, `out-dir`, etc.
>
> ```toml
> build-dir = "/tmp/tsdl"
> out-dir = "/usr/local/lib"
>
> [parsers]
> json = "0.21.0" # The leading v is not necessary
> rust = "master"
> ```

> [!IMPORTANT]
> All configuration specified in `parsers.toml` can be overridden with flags
> passed to `tsdl`, i.e.: `tsdl build --build-dir "/tmp/tsdl"` will
> override whatever value is the default of `tsdl` or in `parsers.toml`.

> [!TIP]
> Check out [Faveod/tree-sitter-parsers](https://github.com/Faveod/tree-sitter-parsers) for an
> example configuration.
