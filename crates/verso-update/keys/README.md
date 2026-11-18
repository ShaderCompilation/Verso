# Release keys

The minisign public keys every Verso build trusts for updates, built into the
binary by `build.rs`:

- `main.pub`: signs every release.
- `backup.pub`: kept offline; signs a release that ships a new main key if the
  main secret key is lost.

Only public keys belong here. The secret keys stay on the developer's machine,
password-protected and backed up. See
[docs/setup.md](../../../docs/setup.md#release-keys) for creating them and
[docs/updates.md](../../../docs/updates.md#signing) for why.

A build without keys can't verify any release.
