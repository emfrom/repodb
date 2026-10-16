# repodb

A repository search CLI with exact and embedding-based search.

Custom embedding commands receive each text on stdin and must print a JSON numeric vector on stdout. RepoDB invokes them as `COMMAND --json -` and sets `REPODB_EMBEDDING_MODEL` to the configured model ID for every indexing and query invocation. Commands must produce vectors consistent with that model ID. The built-in `~/bin/generate_embeddings` supports only `all-MiniLM-L6-v2`.
