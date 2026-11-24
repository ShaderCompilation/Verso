# KPM (Kindle Package Manager) packaging

Verso releases include `verso-<version>.kpkg` alongside `verso-<version>.zip`,
built by `verso-release pack --kpm dist/kindle/kpm` (invoked from
`scripts/package.sh`).

## Package layout (KPM manifest v2)

`verso-<version>.kpkg` is a reproducible gzip-compressed tar archive containing:

- `manifest.json` — KPM v2 metadata (`id: "verso"`, `supported_platforms: ["kindlehf"]`,
  `version: [major, minor, patch]`).
- `files.txt` — sorted list of every relative path installed under `/mnt/us`.
- `install.sh` — installs `payload/` into `/mnt/us`, writes
  `verso/update/hand-install` and `verso/installed.json` last, and preserves
  pre-existing user files under `verso/data/` and `verso/ssh/`.
- `launch.sh` — invokes `/mnt/us/verso/verso.sh` in a detached session
  (waiting for `SH_Integration` to unload if launched through it).
- `uninstall.sh` — handles `$1 = "upgrade"` and full uninstall, removing
  Verso's installed files, KUAL entry and `documents/Verso.sh` while preserving
  user data under `verso/data/` and `verso/ssh/`.
- `payload/` — the same required components, `verso/installed.json` and
  `verso/update/hand-install` shipped in `verso-<version>.zip`.

> **Pre-release versions:** KPM v2's `version` field is strictly a 3-integer
> array `[major, minor, patch]` with no pre-release suffix. For pre-releases
> such as `0.1.0-beta.3`, `manifest.json` encodes `[0, 1, 0]` while
> `payload/verso/installed.json` keeps the exact `"0.1.0-beta.3"` string for
> Verso's in-app updater. Because KPM rejects duplicate `[major, minor, patch]`
> tuples in a repository, publish stable releases (or distinct tuples) to KPM
> repositories.

## Repository manifest (`manifest.v2.json`)

### Standalone repository (e.g. `verso-site` / GitHub Pages)

KPM treats any artifact `url` containing `:` as an absolute URL and follows HTTP
redirects, so a hosted `manifest.v2.json` can point straight at GitHub Release
assets:

```json
{
  "manifest_version": 2,
  "id": "verso",
  "name": "Verso",
  "description": "Official KPM repository for Verso",
  "packages": {
    "verso": {
      "name": "Verso",
      "author": "Verso",
      "description": "A calm, lightweight e-reader for jailbroken Kindles",
      "artifacts": [
        {
          "url": "https://github.com/ShaderCompilation/Verso/releases/download/v0.1.0/verso-0.1.0.kpkg",
          "version": [0, 1, 0],
          "dependencies": [],
          "supported_platforms": ["kindlehf"]
        }
      ]
    }
  }
}
```

Users add and install from that repository with:

```sh
kpm add-repo https://<host>/manifest.v2.json
kpm update
kpm install verso
kpm launch verso
```

### Submitting to `KindleModding/repo`

To add a release to the official KMC repository (which stores `.kpkg` files
under `packages/<id>/artifacts/` and uses relative URLs in `manifest.v2.json`):

```sh
python3 kpm-helper.py repo add /path/to/KindleModding-repo target/package/<version>/verso-<version>.kpkg
```

Or add the `verso` entry directly under `"packages"` in `manifest.v2.json`:

```json
"verso": {
  "name": "Verso",
  "author": "Verso",
  "description": "A calm, lightweight e-reader for jailbroken Kindles",
  "artifacts": [
    {
      "url": "packages/verso/artifacts/verso_0.1.0_kindlehf.kpkg",
      "version": [0, 1, 0],
      "dependencies": [],
      "supported_platforms": ["kindlehf"]
    }
  ]
}
```
