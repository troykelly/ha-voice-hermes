# Container third-party notices

`COMPONENTS.json` is the locked inventory for non-dpkg software copied into
the Home Assistant App image. The listed notice files are verbatim from the
corresponding immutable upstream tags. Multiple skarnet components share a
notice only where their tagged `COPYING` files are byte-identical.

The `workerd-rust/` generated bundle separately covers all registry, git, and
path packages in the exact upstream Cargo lock and records how to obtain MPL
Source Code Form. The `workerd-native/` lock and status cover the conservative
native C/C++ and static-toolchain closure for both exact Linux binaries. Its
normal verifier requires zero unresolved legal obligations while retaining
the two explicitly classified upstream provenance limitations.

The pinned Home Assistant base retains Debian package copyright and licence
records under `/usr/share/doc/<package>/copyright`. The App build verifies that
every installed dpkg package still has that record. Debian corresponding-source
obligations are handled as a separate release-artifact gate; this notice
inventory is not a substitute for corresponding source where a copyleft
licence requires it.

`scripts/verify-container-third-party-notices.py` binds this inventory to the
exact Home Assistant base digest, the exact `workerd` package lock, every
notice hash, both complete workerd gates, the architecture-specific workerd
binary digest check, and the Dockerfile copy destination. A base, runtime,
component, binary, or notice change must update the inventories in the same
reviewed commit.
