# Structural and semantic merge: survey and recommendation

Task: issue #101. All sources were accessed on 2026-10-31. Numbers are quoted from the cited source, never
computed here. "Unverified" marks claims that rest on a secondary summary, a self-reported benchmark, or a page
that could not be fetched. Nothing in this document was run by us.

## Our baseline

The prototype in `crates/vcs-merge/src/structural/` (feature `structural`, Rust only, see `docs/merge.md`) keys
top-level items by kind and name and merges the key sequences with diff3. On its 550-scenario generated corpus
it leaves 150 conflicted files where line diff3 leaves 200, and all of the gain is in "move a function" cases.
The corpus is synthetic and small, so this says nothing about real code. We have no measurement on real merge history.

## Comparison

| Tool | Approach | Languages | Reported results (source) | Merge driver? |
|---|---|---|---|---|
| Mergiraf | tree-sitter, GumTree matching, PCS triples, per-language "commutative parents"; tries line merge first, structured only on conflict | 30 programming languages and 11 file formats per its docs; 33 languages per LWN | Kernel replay: of 7,415 conflicting merge commits, 428 fully resolved, 6,987 still conflicted ([LWN][lwn]); informal, author says it is an underestimate. Java-only academic comparison below | Yes, Git driver and `mergiraf solve` |
| Spork | Java only; Spoon AST, GumTree, 3DM; falls back to line merge; preserves formatting | Java | Paper replays 1,740 file merges from 119 projects; abstract gives no conflict-reduction number ([arXiv][spork]) | Yes (`--git-mode`) |
| jDime | Java structured merge, auto-tuned: unstructured until a conflict, then structured | Java | ASE 2015: 50 projects, 434 scenarios; structured was an order of magnitude slower, auto-tuning up to 92x and about 10x on average faster than pure structured ([search summary][jdime], unverified against the PDF) | Research tool |
| LastMerge | Generic structured merge configured via a thin interface; compared with jDime and Spork | Evaluated on Java only | Abstract: about 10% divergence between Java-specific and generic tools; LastMerge 15% fewer false positives than jDime; Mergiraf misses 42% fewer false negatives than Spork; no evidence generic merge hurts accuracy ([arXiv][lastmerge]) | Research tool |
| Weave | tree-sitter entity-level (function, class, JSON key) driver in Rust; re-parses output, falls back to line merge | about 38 per its README | Self-reported only: 31 hand-written scenarios (Weave 31/31 correct, Mergiraf 0.16.3 28/31, Git 17/31); replay of first 500 merge commits in each of 5 repos: 344 wins and 86 regressions over 4,971 file merges ([README][weave]). Repo blurb claims about 95% reduction, unexplained, treat as unverified | Yes |
| difftastic | tree-sitter syntactic diff | 20+ languages | Diff only, no merge ([site][difft]) | No |
| SemanticMerge | Language-aware merge/diff with move detection, from Codice (Plastic SCM) | C#, Java, others | Standalone product ended; a forum thread dated 2022-04-05 says so, but the body could not be fetched, so details are unverified ([search result][semmerge]). Closed source, so not an option | n/a |

Structured-versus-semistructured study (ASE 2019, more than 40,000 scenarios): the two differ on 24% of conflicting
scenarios; semistructured reports more false positives, structured more false negatives ([search summary][ase19],
unverified against the paper).

## What the evidence says

1. No source supports a headline conflict-reduction figure that transfers to our case. The only independent
   large replay (LWN, Linux kernel, mostly C) shows a small share fully resolved. The strongest numbers are
   Weave's, and they are self-reported.
2. The academic work (jDime, Spork, LastMerge) is Java-only. Generic tree-sitter tools were found comparable in
   accuracy to Java-specific ones there, but the added false positives of Mergiraf (more spurious conflicts than
   Spork) and false negatives (silently wrong merges) are the trade-off to track. I did not verify the raw counts.
3. Every production-minded tool shares one design: try the cheap line merge first, run structured merge only on
   conflict, parse-check the result, and fall back to line markers on any doubt. Our prototype already has the
   parse check and the decline path; it lacks the tree matching that makes edit-plus-move work.
4. Language breadth comes from tree-sitter grammars plus a small per-language table (Mergiraf: commutative
   parents and signatures). That is cheaper than hand-writing item extraction per language as we do for Rust.

## Recommendation

Adopt:
- Keep the line-first, structured-on-conflict ordering and the parse-validity guard (already in the prototype).
- Replace "item key by kind and name" with a generic tree-sitter tree plus a per-language table
  (commutative parents, signatures). This is the Mergiraf/LastMerge design and extends past Rust.
- Treat a structured clean merge as a candidate, validated by parse and, per `docs/research.md` M6 notes, by tests.

Avoid:
- A hard dependency on any one external tool in core crates. Mergiraf is GPLv3 (per [LWN][lwn]); linking its
  code would constrain our license. Weave is MIT or Apache-2.0 (per its README) but young and self-benchmarked.
- Quoting a reduction percentage from any vendor README in our docs.
- SemanticMerge (closed, discontinued) and difftastic (diff only).

## Concrete next steps

1. Build a real-history replay harness: take merge commits from public repos, compare line merge, our
   prototype, and the human result. This yields our own measured numbers (issue to be filed).
2. Run Mergiraf and Weave as external drivers on that same corpus as reference points, outside core crates.
3. Prototype generic tree-sitter matching (top-down identical subtrees, then bottom-up) plus a commutative-parent
   table for a second language, to test whether the generic route beats per-language item keys.
4. Add a false-accept metric (clean structural merges that differ from the human result), since the literature
   shows the risk is silent wrong merges, not only leftover conflicts.

## Sources

- [lwn]: https://lwn.net/Articles/1042355 (accessed 2026-10-31)
- Mergiraf home, languages, architecture: https://mergiraf.org/ , https://mergiraf.org/languages.html , https://mergiraf.org/architecture.html (2026-10-31)
- [spork]: https://arxiv.org/abs/2202.05329 and https://github.com/ASSERT-KTH/spork (2026-10-31)
- [lastmerge]: https://arxiv.org/abs/2507.19687 (2026-10-31); dataset size not visible in the abstract
- [weave]: https://github.com/Ataraxy-Labs/weave (2026-10-31)
- [difft]: https://difftastic.wilfred.me.uk/ (2026-10-31)
- [jdime]: https://www.se.cs.uni-saarland.de/projects/jdime/ and https://www.se.cs.uni-saarland.de/publications/docs/LAL15ase.pdf (2026-10-31, found via search, not fetched)
- [ase19]: https://www.se.cs.uni-saarland.de/conferences/ASE/ase2019/details/ase-2019-papers/23/The-Impact-of-Structure-on-Software-Merging-Semistructured-versus-Structured-Merge.html (2026-10-31, search summary only)
- [semmerge]: https://forum.plasticscm.com/topic/23356-ended-the-availability-of-standalone-semantic-merge/ (2026-10-31, redirected, body not read)

[lwn]: https://lwn.net/Articles/1042355
[spork]: https://arxiv.org/abs/2202.05329
[lastmerge]: https://arxiv.org/abs/2507.19687
[weave]: https://github.com/Ataraxy-Labs/weave
[difft]: https://difftastic.wilfred.me.uk/
[jdime]: https://www.se.cs.uni-saarland.de/projects/jdime/
[ase19]: https://www.se.cs.uni-saarland.de/conferences/ASE/ase2019/details/ase-2019-papers/23/The-Impact-of-Structure-on-Software-Merging-Semistructured-versus-Structured-Merge.html
[semmerge]: https://forum.plasticscm.com/topic/23356-ended-the-availability-of-standalone-semantic-merge/
