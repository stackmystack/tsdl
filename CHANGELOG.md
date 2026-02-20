# Changelog

## [2.0.0] - 2026-02-20

This is a major rewrite, moving to a single-threaded async runtime, with actors.
([aacb30f](https://github.com/stackmystack/tsdl/commit/aacb30f3f4cc3979ab9fdd074658acfe7bab9970))

Some of the things done are arguably unnecessary, granted, but also as always,
it's a serious project where I get to try and learn to do things at the same
time.

Some people call it fun, believe it or not …

### Features

- **cache**: _new_ Skips rebuilding when no changes are found
  - Skip useless rebuilds and --force to override - ([5910ad7](https://github.com/stackmystack/tsdl/commit/5910ad771f7be9c16297f5392580887202916651))
  - Handle targets of --target=all - ([2a0d445](https://github.com/stackmystack/tsdl/commit/2a0d44557909f38cd83f4d1752b35768a6ae9977))
- **lock**: _new_
  - Output dir - ([5aeb1d7](https://github.com/stackmystack/tsdl/commit/5aeb1d787cd953bd4095dec07f618958f1010795))
- **cli**: Tag each downloaded tree-sitter cli with its proper version - ([a8987d9](https://github.com/stackmystack/tsdl/commit/a8987d900f262b61acce2fb6dc7134fe7adb56c6))

### Bug Fixes

- **build**: Generate file name with prefix and extension on build - ([0e2a8b4](https://github.com/stackmystack/tsdl/commit/0e2a8b411d768250d62fd0101e4b58124d982cfb))
- **build**: Copy exact files, or best match when possible - ([c1cce92](https://github.com/stackmystack/tsdl/commit/c1cce92e0c6a18ccda6383a0981b81b8282042c2))
- **build**: --force = --fresh when installing - ([55b07a4](https://github.com/stackmystack/tsdl/commit/55b07a4673f9e176fd47cb3cd05ef6c677fff9bc))
- **display**: Integer math - ([42ad2d0](https://github.com/stackmystack/tsdl/commit/42ad2d083add96f752f87fc6d101b433dcaf4d8a))

## [1.5.0] - 2025-04-08

### Features

- **target**: Use --target [native, wasm, all] - ([77f965e](https://github.com/stackmystack/tsdl/commit/77f965e86426b92cb39defb9ec17ceacb5474a5b))

### Bug Fixes

- **cliff**: Remove new contributors section - ([f655e6c](https://github.com/stackmystack/tsdl/commit/f655e6cb16bde0564aa2c8114fa2e3e0eb15aeea))
- **release**: Do not add a leading v when deducing version - ([80a7993](https://github.com/stackmystack/tsdl/commit/80a7993bd6dc38db0058540dca923cfd8d6e8351))
- **tree_sitter**: Avoid needless iteration - ([c7b08cb](https://github.com/stackmystack/tsdl/commit/c7b08cb8fcb86880cbecce667bd2fc80920b5b1f))
- Add support for `--target`:
  - `native` for native shared libraries, `dylib` or `so` (default).
  - `wasm` for web assembly.
  - `all` for all of the above.

## [1.4.0] - 2025-03-22

### Features

- **clone**: Always clone to tree-sitter-{language} dir - ([daf6e36](https://github.com/stackmystack/tsdl/commit/daf6e3683117354d7dfacd7c9d4807ab33955737))

### Other

- **tree_sitter**: Init - ([160d832](https://github.com/stackmystack/tsdl/commit/160d8325cf689b7947823a7fba099b0390d0166b))

## [1.3.1] - 2025-01-28

### Bug Fixes

- **cargo.toml**: Specify markdown_to_docs version for proper publishing - ([0b9d96a](https://github.com/stackmystack/tsdl/commit/0b9d96aee3f8cf0f5051b2f84a2178b9c77c00a8))

## [1.3.0] - 2024-11-21

### Features

- **config**: From env vars if cmd args are not present - ([b693719](https://github.com/stackmystack/tsdl/commit/b6937196d4b9c5c3e61b462fdc7742b10efa98fa))

### Bug Fixes

- **build**: Clean, clone, then build if remote definition changes - ([62375ae](https://github.com/stackmystack/tsdl/commit/62375ae753b3749fd0605c45c344d115e116c6e3))
- **clone**: When a parser was not correctly cloned in a previous run - ([b976d87](https://github.com/stackmystack/tsdl/commit/b976d8762d4e540d274366ba16cf918f9b2706e3))

### Documentation

- **crate**: Add README.md to the crate's documentation - ([8d25198](https://github.com/stackmystack/tsdl/commit/8d251987413c8e8c6481831525a0d5b893021ef7))
- **tsdl**: Use markdown_to_docs to properly display in docs.rs - ([e980e99](https://github.com/stackmystack/tsdl/commit/e980e9959dfdfc2c76cb5407fe71c18e099e460a))

### Other

- **markdown_to_docs**: Remove unnecessary reference - ([1e5f8d1](https://github.com/stackmystack/tsdl/commit/1e5f8d1d64949e71f7b1c88dcdcff61f86025886))
- Style - ([8bfc86b](https://github.com/stackmystack/tsdl/commit/8bfc86bfa3d3489adb197f69d7d247af2a5aebd1))

### Bug Fixes

- **build**: --tree-sitter-version sha1 - ([a611f94](https://github.com/stackmystack/tsdl/commit/a611f94ed98bce297fd4af9fc5a2ccdb55925941))
- **download**: Remove tree-sitter-cli gz - ([69af2a4](https://github.com/stackmystack/tsdl/commit/69af2a4d5b887fef7cc075bbb3603e2536a8b71a))
- **log**: Create dir if --log specifies a path - ([1d88722](https://github.com/stackmystack/tsdl/commit/1d887223cd56a71ce5f00b798b5da3194cc192f2))

### Documentation

- **cli**: Remove wrong description for build command - ([c73f096](https://github.com/stackmystack/tsdl/commit/c73f096ca67149245d74d73e65731b9a2ae22a0a))

### Other

- **build**: Check that tree-sitter-cli exists and is executable - ([5abb1a5](https://github.com/stackmystack/tsdl/commit/5abb1a5fc941d4acd9f382481d38925e514cd66c))
- **build**: Check downloaded tree-sitter-cli version - ([362dc5c](https://github.com/stackmystack/tsdl/commit/362dc5c5d6d9620487af38ec53b3b822bfebfda7))
- **build**: Verify multiparser with cmd - ([35e8442](https://github.com/stackmystack/tsdl/commit/35e8442456c1d51af03eabb37b1dcd52e93c0023))
- **lint**: Add typos checker - ([70cd5ce](https://github.com/stackmystack/tsdl/commit/70cd5ce8597816768658e6a7ba1e2fdd8880bf07))

## [1.2.0] - 2024-09-06

### Features

- **selfupdate**: Add selfupdate command and fetch from github releases - ([a0832d8](https://github.com/stackmystack/tsdl/commit/a0832d86316e5af7c9c64230ff387e9fae01db48))

### Bug Fixes

- **chmod**: Use rust to set executable permissions - ([a36e1d9](https://github.com/stackmystack/tsdl/commit/a36e1d94b75e45887aeb87849789e7d7dec39be2))
- **download**: Use reqwest instead of curl or wget - ([76ac6c9](https://github.com/stackmystack/tsdl/commit/76ac6c9a36e2737e626e01300269c5ff43437290))
- **gunzip**: Use async-compression instead of the gunzip binary - ([de16590](https://github.com/stackmystack/tsdl/commit/de165904bea4adfd264a57aa6778c61437e9911d))
- **release.sh**: Lint and test before releasing - ([bc5aa6c](https://github.com/stackmystack/tsdl/commit/bc5aa6ccb77dea9d784bacaad8642bc04ccb4f86))

### Documentation

- **readme**: More information on config file and overriding parser.toml - ([dcafb2c](https://github.com/stackmystack/tsdl/commit/dcafb2ccc26eac7d3716ee501bb71517eb55d23f))

## [1.1.0] - 2024-09-04

### Features

- **logging**: Print to stderr and file when -v,-vv - ([181cf77](https://github.com/stackmystack/tsdl/commit/181cf77bc03da1cad46335246700c62d6d9cb036))

### Documentation

- **changelog**: Simplify and skip chore, refactor, and style commits - ([dccf215](https://github.com/stackmystack/tsdl/commit/dccf2156d4721d46dbdef904d783d95cbe4b069f))

### Other

- **pr template**: Remove the git-cliff message - ([1507e3e](https://github.com/stackmystack/tsdl/commit/1507e3ed6dbd2e12ed6081299091a343f14a5411))
- **test**: Retry because some tests are flaky - ([c44183b](https://github.com/stackmystack/tsdl/commit/c44183b27832a1cd6ce39a7e7e1edf52a25162f3))

## [1.0.0] - 2024-09-01

### Features

- **tsdl**: Working implementation

<!-- generated by git-cliff -->
