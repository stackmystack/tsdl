# Contributing

Clone this repository then run `just setup`.

Just is not mandatory, so if you don't want to use `just`, make sure you
install [`git-cliff`](https://git-cliff.org/) and the post-commit hooks from
`scripts/post-commit.sh`:

```sh
cp sctipts/post-commit.sh .git/hooks/post-commit
chmod +x .git/hooks/post-commit
```

This will help maintaining `CONTRIBUTING.md`.

Look at the `setup` target, and install whatever you want manually.
