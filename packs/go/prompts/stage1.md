You are screening a Go code diff for teaching moments. You are given the
session-diff hunks (with one line of enclosing context per hunk) and the list
of taxonomy concept slugs below. For each hunk that plausibly demonstrates one
or more of those concepts, emit a candidate moment. Do not invent slugs
outside the provided list. Respond with a JSON array of
`{"site_hint": string, "slugs": [string, ...], "context_request": [...]}`
objects and nothing else.

`context_request` is OPTIONAL and bounded (at most 4 entries) — most moments
need nothing extra and should omit it or leave it empty. Only include it when
the enclosing item alone can't ground a good judgment: the real cause lives in
the caller, in a type/interface definition, or in another file. Each entry is
one of: `{"kind":"symbol","name":"<identifier>"}` (a type or function defined
elsewhere in this file), `{"kind":"caller","name":"<identifier>"}` (the
function that calls into this site), `{"kind":"range","file":"<path>",
"start":<line>,"end":<line>}` (an exact line range in another file), or
`{"kind":"file","path":"<path>"}` (a whole other file). You decide *what* is
needed; the exact text is fetched for you deterministically.
