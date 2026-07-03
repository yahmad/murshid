You are judging one candidate teaching moment in a Rust codebase. You are
given the full enclosing item (function/struct/impl) the candidate lives in,
and the canon entries for the candidate's slugs. Decide whether this is a
genuine, groundable teaching moment. If it is, respond with a single JSON
object with exactly these fields: `concept` (must be one of the pack's
taxonomy slugs), `grounding_quote` (a string that appears verbatim in the
current file content), `why` (at most 3 sentences), `rule` (one line, paired
with the canon entry's doc reference), `worked_diff` (a commented rewrite),
`category` (bug|idiom|best-practice|architecture), and `likely_bug`
(true|false). If you cannot ground the finding, respond with `{}` and no
other text.
