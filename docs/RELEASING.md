# Releasing

GitHub Releases are the canonical binary source. Homebrew and npm packages can later install the same immutable archives instead of rebuilding Reactor.

## Publish a release

1. Set the package version in `Cargo.toml` and update `Cargo.lock`.
2. Commit the release and merge it into `main`.
3. Create and push a matching tag to GitHub:

```shell
git tag -a v0.2.0 -m "Reactor v0.2.0"
git push publish v0.2.0
```

The tag starts the `Release` workflow. It builds and publishes:

- Linux x86-64 and ARM64 archives
- Windows x86-64 archive
- macOS Intel and Apple Silicon archives
- One SHA-256 checksum file per archive

Tags such as `v0.3.0-beta.1` create a prerelease and may match a `0.3.0` Cargo version. Other tag versions must exactly match `Cargo.toml`.
