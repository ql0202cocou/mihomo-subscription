# sqlx-sqlite

Source: the `sqlx-sqlite` 0.9.0 package published on crates.io.
Package SHA-256: `488e99c397a62007e4229aec669a179816339afc6d2620ca6fa420dbee2e982c`.
The package's license entries refer to files in the upstream repository root.
The full MIT and Apache-2.0 license texts are included here, copied from the
[package's source commit](https://github.com/transact-rs/sqlx/tree/003b698e99e024f3621b8043a2426fde5b741171).

The Rust source is unchanged. `Cargo.toml` and `Cargo.toml.orig` widen the
`libsqlite3-sys` upper bound from `<0.38.0` to `<0.40.0`, following upstream's
[0.39.x support change](https://github.com/transact-rs/sqlx/commit/10462fa4414c6c7f1ed1d39bb31678e3b726878c).
The package's generated lockfile and registry cache metadata are omitted;
the application uses its root `Cargo.lock`.

Remove this copy and the root `[patch.crates-io]` entry when a published SQLx
0.9 release supports the selected `libsqlite3-sys` version. Remove the Docker
`COPY vendor` step if no other vendored dependencies remain.
