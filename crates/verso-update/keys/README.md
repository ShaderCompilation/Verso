# Release keys

The minisign public keys every Verso build trusts for updates, built into the
binary by `build.rs`:

- `main.pub`: signs every release.
- `backup.pub`: kept offline; signs a release that ships a new main key if the
  main secret key is lost.

Only public keys belong here. The secret keys stay on the developer's machine,
password-protected and backed up. Create each pair with `minisign -G -p
crates/verso-update/keys/NAME.pub -s ~/.minisign/verso-NAME.key`. The backup
key only signs a release that ships a new main key; if both secret keys are
lost, everyone has to update by hand once.

A build without keys can't verify any release.
