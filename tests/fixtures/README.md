# Legacy CLI fixture

`legacy.sqlite` is an unmodified database produced by RepoDB #3 (`a1a4e56`)
using `init` and `reembed`, with deterministic fake vectors and model
`legacy-model`. Its command is the repository-relative `./legacy-embedder`.
No SQL was used to create or inspect it.

Regenerate from the repository root with `sh tests/fixtures/generate-legacy.sh`.
The source repository contains only `legacy.txt` (`legacy marker\n`) at commit
`dc090cb6bf0c933d305d3cab0b0c49c75845990a`. The CLI migration test recreates that
commit and verifies old/new search behavior, including failed refresh, without
opening the database through an internal seam.
