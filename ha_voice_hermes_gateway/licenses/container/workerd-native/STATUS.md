# Native workerd licence gate

This directory is the conservative native C/C++ notice inventory for the
exact `workerd` `1.20260708.1` Linux packages. Rust crates are deliberately
outside this inventory.

## Legal gate

The native notice/source gate has zero known legal blockers. `LOCK.json`
binds the workerd source commit, NPM integrity, both architecture package and
binary SHA-256 values, the exact Bazel derivation files, every named native
source revision/archive digest, and every vendored notice digest.

The inventory deliberately includes all licence-like files found in the
pinned component trees, including build, benchmark, test, and packaging
notices that binary feature selection may not require. This over-inclusion
avoids treating uncertain feature selection as permission to omit a notice.

Perfetto is not represented by only its root Apache text. Its complete pinned
tree has two licence files plus legal metadata, all of which are included. The
exact target chain used by workerd is also vendored: workerd enables
`libperfetto_client_experimental`, that target selects Perfetto's regex target,
and the standalone regex configuration selects RE2. RE2 and its transitive
Abseil dependency are therefore first-class components. Protobuf 35 is used by
`protoc` to generate Perfetto's protozero C++ during the build; no protobuf
runtime is linked into the distributed executable.

The two Linux binaries are unstripped. Inspection of their dynamic sections,
symbols, and embedded source paths found:

- only the system loader, `libc`, and `libm` as dynamic dependencies;
- native repository markers matching the locked named closure;
- direct markers for the compression, ICU, SQLite, and SIMD libraries; and
- no bundled glibc trigonometric symbols from V8's optional LGPL-covered
  source directory. The V8 glibc licence and the exact V8 source archive are
  still included conservatively.

The upstream release configuration statically links `libc++.a` and libgcc.
The exact workflow, setup action, linker flags, toolchain comments, and static
symbols are locked as evidence. LLVM libc++, libc++abi, libunwind and
compiler-rt 19.1.7 licence and credit files are included from the commit
printed by the binary. GCC 12.3.0's complete root COPYING family, including
the GPL text and Runtime Library Exception, is also included; eligible runtime
output carries no corresponding-source obligation under that exception.

No mandatory corresponding-source obligation was found in this native
closure. Zstandard is selected under its BSD-3-Clause option. Dynamic `libc`,
`libm`, the loader, and glibc startup coverage belong to the container-wide
Debian notice/corresponding-source gate; the native lock records that ownership
explicitly instead of silently treating system/runtime code as part of
workerd's source tree.

## Residual limitations

Cloudflare's NPM package does not ship a signed native SBOM, build-feature
attestation, or reproducible-build proof. That is a supply-chain evidence
limitation, not evidence of a missing licence or source obligation after the
conservative source-graph and binary review above. The two limitations remain
explicit in `LOCK.json` and are verified so they cannot be silently deleted or
misclassified.

If later evidence identifies another compiled native repository, it is a
concrete legal blocker: add its exact source pin, all relevant notices, source
obligation decision, and binary evidence before release.

## Commands

Offline release verification:

```sh
python3 scripts/verify-workerd-native-notices.py
```

Network comparison against the immutable upstream inputs:

```sh
python3 scripts/refresh-workerd-native-notices.py --check
```

Refresh after deliberately reviewing an updated lock:

```sh
python3 scripts/refresh-workerd-native-notices.py --update
```
