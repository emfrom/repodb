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

# Version 2 CLI fixture

`v2.sqlite` is an unmodified database produced by RepoDB #4 (`3c58c0e`)
using public `init` and `reembed` commands, deterministic `[1,0]` vectors,
model `v2-model`, and repository-relative command `./v2-embedder`.
No SQL was used to create or inspect it.

Regenerate offline with `sh tests/fixtures/generate-v2.sh`. The source repository
contains only `v2.txt` (`v2 first\nv2 marker\nv2 last\n`) at commit
`0c729620cba6ed31c8f6ef1384e8bb34631eab98`. The CLI test recreates that commit,
checks that the old line units and vectors survive searches and failed refreshes,
then verifies paragraph units and rebuilt vectors after successful refresh.
