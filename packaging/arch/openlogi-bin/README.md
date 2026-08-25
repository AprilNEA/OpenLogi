# openlogi-bin (AUR)

Template for the [`openlogi-bin`](https://aur.archlinux.org/packages/openlogi-bin)
AUR package. It repackages the released `.pkg.tar.zst` for both architectures —
a source build is impractical in makepkg because GPUI is a rev-less git
dependency on the zed monorepo, pinned only by `Cargo.lock`.

`release.yml`'s `aur-publish` job renders and pushes it on every stable tag:
`@PKGVER@` becomes the tag without the `v`, and the hash placeholders take the
matching lines from the release's `SHA256SUMS` — so the AUR hashes are always
the upstream-attested ones, never recomputed. To render by hand:

```sh
sed -e 's/@PKGVER@/0.8.0/g' \
    -e 's/@SHA256_X86_64@/<amd64 pkg sha256>/g' \
    -e 's/@SHA256_AARCH64@/<arm64 pkg sha256>/g' PKGBUILD
```

The job is dormant: it runs only when the repository variable
`AUR_PUBLISH_ENABLED` is `true`, and it loads the SSH key from the 1Password
item referenced by the `OP_AUR_SECRET_ITEM` secret (field
`AUR_SSH_PRIVATE_KEY`, base64-encoded, unencrypted key). **Do not enable it
before the co-maintainer cutover with the current `openlogi-bin` maintainer
(njkevlani — see #255 / #265):** their pipeline updates the package today, and
two writers to one AUR repo race each other.

`provides=(openlogi=$pkgver)` / `conflicts=(openlogi)` keep the package
interchangeable with a future repo or source `openlogi` package.
