You are screening a Rust code diff for teaching moments. You are given the
session-diff hunks (with one line of enclosing context per hunk) and the list
of taxonomy concept slugs below. For each hunk that plausibly demonstrates one
or more of those concepts, emit a candidate moment. Do not invent slugs
outside the provided list. Respond with a JSON array of
`{"site_hint": string, "slugs": [string, ...]}` objects and nothing else.
