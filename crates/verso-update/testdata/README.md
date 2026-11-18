# Test fixtures

`latest.json` and the two archives are a small release made by `verso-release pack`. The signatures
over it were made with two throwaway test keys whose secret halves were
deleted; `test.pub` and `other.pub` are never trusted by a build.

| File | Signed with | Trusted comment |
|---|---|---|
| `latest.json.minisig` | test | `verso 0.3.0` |
| `latest.json.other.minisig` | other | `verso 0.3.0` |
| `latest.json.comment.minisig` | test | `verso 0.2.0` (wrong version) |
| `latest.json.legacy.minisig` | test, legacy format (`-l`) | `verso 0.3.0` |

To change `latest.json`, make new keys and signatures:

```sh
minisign -G -W -f -p test.pub -s test.key
minisign -G -W -f -p other.pub -s other.key
minisign -S -s test.key -m latest.json -t "verso 0.3.0" -x latest.json.minisig
minisign -S -s other.key -m latest.json -t "verso 0.3.0" -x latest.json.other.minisig
minisign -S -s test.key -m latest.json -t "verso 0.2.0" -x latest.json.comment.minisig
minisign -S -l -s test.key -m latest.json -t "verso 0.3.0" -x latest.json.legacy.minisig
rm test.key other.key
```
