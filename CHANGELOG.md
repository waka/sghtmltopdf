# CHANGELOG

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Fixed

- Make the `ruby` platform gem a placeholder that installs without building. It previously declared a native extension it could never build, so `bundle install` died on platforms with no precompiled gem — and died with a confusing `cannot load such file -- rb_sys/mkmf`, since `rb_sys` is only a development dependency.
- Requiring the placeholder now raises a `LoadError` that names the supported platforms, instead of failing on a missing `.so`.

### Changed

- A `Gemfile.lock` listing only the `ruby` platform is now all that is needed; bundler resolves it to the precompiled gem for whichever platform it installs on, so `bundle lock --add-platform` is not required and the lockfile is left unchanged.
- Drop the Rust sources and `Cargo.{toml,lock}` from the `ruby` platform gem, which could not be built from them anyway.

## 0.1.1

### Changed

- Change gemspec from Japanese to English.
- Change required Ruby version for precompiled gem.

## 0.1.0

### Added

- 1st release on 2026-08-08
