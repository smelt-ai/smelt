# Smelt patch

This is the crates.io `block` 0.1.6 source, vendored because GPUI's macOS dependency chain still requires that exact legacy crate.

The source changes are limited to Rust compatibility fixes in `src/lib.rs`: the opaque Objective-C `Class` marker is represented by an inhabited zero-sized struct instead of an uninhabited enum, and legacy `extern` declarations explicitly state their existing C ABI. Rust warns that an extern static of an uninhabited type will become a hard error (rust-lang/rust#74840). The symbol is only used by address, so these changes preserve its ABI and behavior.

Remove this override when the GPUI dependency chain no longer uses `block` 0.1.6.
