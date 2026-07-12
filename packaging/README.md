# Packaging Notes

The packaged application identity is `dev.charlesmills.gmpublished`.

## Steam Runtime Libraries

Packaging stages the Steam runtime library for each target platform:

- `windows/steam_api64.dll`
- `macos/libsteam_api.dylib`
- `linux/libsteam_api.so`

## Linux File Association Maintenance

`cargo-packager` 0.11.8 does not expose Debian maintainer script or Pacman
install hooks for refreshing desktop and MIME databases. Package consumers may
need to run:

```sh
update-desktop-database /usr/share/applications
update-mime-database /usr/share/mime
```

## macOS Universal Builds

The release macOS CI job builds a universal app without changing the packager
configuration. It stages both architectures first, then sets
`GMPUBLISHED_SKIP_PREPARE_BUILD=1` so the packager consumes the staged binary
instead of rebuilding a host-only binary.

The local macOS smoke path is still `just bundle` or `just run-bundle`; it
remains a host-architecture local smoke. For faster local UI iteration, use `just bundle-dev` or `just run-bundle-dev`.
