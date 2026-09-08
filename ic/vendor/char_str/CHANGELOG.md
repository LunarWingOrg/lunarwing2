# Changelog

<!-- Ref: https://keepachangelog.com/en/1.1.0/ -->

## [Unreleased]

## [0.0.2] - 2026-07-16

### Added

- Added `CharStr::new_inline`, `CharStr::new_heap`, and `CharStr::try_new_heap`
  for explicit control over string storage. ([#28](https://github.com/astral-sh/char_str/pull/28))

## [0.0.1] - 2026-07-15

### Changed

- Forked [`lean_string` 0.6.1](https://github.com/ryota2357/lean_string/releases/tag/v0.6.1)
  as `char_str` for use in Ruff and ty.
- Renamed `LeanString` to `CharString` and `LeanStr` to `CharStr`.
- Added optional `salsa` and `get-size` integrations for ty.

[Unreleased]: https://github.com/astral-sh/char_str/compare/v0.0.2...HEAD
[0.0.2]: https://github.com/astral-sh/char_str/compare/0.0.1...v0.0.2
[0.0.1]: https://github.com/astral-sh/char_str/releases/tag/0.0.1
