# Dokany Rust Wrapper

This fork binds Rust applications to an explicitly selected [Dokany distribution profile](https://github.com/yonenki/dokany). It consists of two crates:

- [![crates.io](https://img.shields.io/crates/v/dokan-sys)](https://crates.io/crates/dokan-sys) `dokan-sys` provides raw bindings to the functions and structures provided by Dokan.

- [![crates.io](https://img.shields.io/crates/v/dokan)](https://crates.io/crates/dokan) `dokan` is built on top of `dokan-sys` and provides high-level, Rust-friendly wrappers for Dokan.

Generally, it is recommended to use the `dokan` crate, which has the unsafe raw bindings wrapped and is easier to use. However, if you want to access the low-level interface provided by Dokan, `dokan-sys` can save you from writing the function and structure definitions yourself.

# Build

`dokan-sys`, which is also a dependency of `dokan`, generates a validated distribution profile and builds the matching user-mode DLL and import library from the bundled Dokany source. The default profile is `profiles/upstream.json`, preserving the upstream Dokany family.

The build requires .NET 8, Visual Studio C++ Build Tools, and a Windows SDK. It never discovers or links an installed official Dokan library. This prevents a developer or user machine from silently selecting a different driver family.

Set `DOKAN_DISTRIBUTION_PROFILE` to an explicit external profile JSON path to build another family. Set `DOKAN_DLL_OUTPUT_PATH` to copy the generated DLL, with version resources, into an application staging directory. Both the native library and Rust crate receive the same generated profile hash, protocol ABI, names, and family identity.

Applications can call `verify_runtime_identity` before mounting. It rejects a driver whose schema, protocol ABI, capabilities, or profile hash do not match the linked DLL. Mount creation also performs the native fail-closed identity check.

# Usage

- `dokan-sys` exposes the native API plus the fixed-layout runtime identity protocol. Read [Dokan's documentation](https://dokan-dev.github.io/dokany-doc/html/) for the base API.
- `dokan` has [detailed documentation](https://dokan-dev.github.io/dokan-rust-doc/html/dokan/) and a [memfs example](https://github.com/dokan-dev/dokan-rust/tree/master/dokan/examples/memfs) available. You can also find some examples in [the unit tests](https://github.com/dokan-dev/dokan-rust/blob/master/dokan/src/tests.rs) and existing projects like [yasfw](https://github.com/DDoSolitary/yasfw).
