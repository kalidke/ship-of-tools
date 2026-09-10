---
name: sitrep
description: Situation report — a plain-language narrative of the last work effort for the person steering the project, in one causal chain (the issue and its context, how it was diagnosed, what the cause is, what test or fix was built, the quantitative result, what it means, what to do next). Invoke on "sitrep", "/sitrep", "what did you do", "where are we", "summarize the last effort", and fire it yourself as the closing message whenever a work effort finishes. Two levels: the chat sitrep (about 300 words) and the log executive summary (about 120 words) that opens an effort or experiment page.
---

# sitrep — say what was done and why, in the reader's language

The reader steers the project but does not live inside it. They read the chat
and the project log intermittently, know the scientific field well, and do not
carry the project's private vocabulary. A sitrep lets them, in two minutes,
know what the most important open problem is, what was just learned about it,
whether the numbers mean anything, and what they are being asked to decide.

A sitrep is not a progress report and not a technical report. Activity
(commits, files, agents, hashes, run names) is never the content. Meaning is.

## When to write one

- The user asks (`/sitrep`, "what did you do", "where are we", "summarize").
- A work effort closes: a delegated worker returns, an experiment finishes, a
  benchmark run is scored, a decision point is reached. The closing message of
  that effort IS the sitrep. Do not report the close in any other shape first.
- A direction changes or a previous conclusion is withdrawn. Say so in the
  sitrep, plainly, as part of the chain.
- NOT for a step in a live back-and-forth (owner ruling 2026-09-10). When the
  user is steering turn by turn — "change this", "what about that", a quick
  answer — reply in the register of the exchange and end; no closing block.
  The block belongs to an effort's close and to a parked end. The Stop hook
  asks once after a long turn; if the turn was a step, say nothing and end.

One sitrep per effort. If several efforts closed since the reader last looked,
write one sitrep that carries the chain across all of them, not one per effort.

## The shape

One narrative in causal order. Paragraphs, not headers. Each link answers the
question the previous one raises. Do not follow the wording literally; follow
the chain.

1. **The issue in its context.** What the project is trying to do, in one
   sentence, and the most significant remaining obstacle, in one or two. A
   reader who arrives cold must be oriented here.
2. **The diagnosis.** How the cause was diagnosed (what was compared with what,
   what control was run) and the cause itself, described conceptually in the
   field's own terms. If a candidate cause was ruled out, say so and say why.
3. **The design.** What test or fix was built, conceptually, and why that
   design distinguishes the candidate causes or repairs the diagnosed one.
4. **The result.** Quantitative, with the unit, the scale, and the noise floor
   or reference it must beat. A number without its scale is not a result.
5. **The interpretation.** What the result means for the diagnosis and for the
   project, in plain words. A negative result is a result; say what it rules
   out. A withdrawn conclusion is named, not buried.
6. **The plan.** The suggested next action, and what the reader must decide,
   if anything. If a decision is theirs, say exactly what is being asked.

## Language rules

- **Every project-coined term is replaced by its plain description, or defined
  in the same sentence on first use.** The test is: a colleague in the field
  who has never seen this project understands the sentence. Names of internal
  test conditions, scores, phases, runs, or components ("rung", "pin", "T4",
  "warmup", "estimator", "β") do not appear bare. Say "the hardest test movie,
  which contains 19 molecules", not "T4, truth 19".
- **No identifiers in prose.** No commit hashes, file paths, function names,
  agent or worker names, run directories, environment variables. If the
  reader must go somewhere, name one place at the end, once.
- **Numbers carry meaning.** Give the unit, the scale (a score out of what),
  the reference value, and the noise it must exceed, in the same sentence.
  Prefer "recovered about 6 points on a 100-point track-quality score, below
  the 8-point run-to-run noise" to "+0.064 β". Never more than two or three
  numbers per paragraph; the rest go to the log page.
- **Say what changed for the project, not what activity occurred.** "The
  benchmark score did not move" beats "the pin stays at v2". "The defaults are
  unchanged; the new step is available as an option" beats "both stay opt-in".
- **One idea per sentence.** Short sentences. No bullet lists in the chat
  sitrep: the chain is prose because the causality is the point.
- **Honesty about confidence.** If a step rests on one run, or on a comparison
  that could be an artefact, say so in the sentence that reports it.
- **Length.** Chat sitrep about 300 words, never more than 400. Log executive
  summary about 120 words. Shorter is better if the chain is complete.

## Two levels, one chain

The chat sitrep and the log's executive summary tell the same chain at
different densities. The chat version explains; the log version states and
links. Write the chat version first; the log version is its compression, with
the detailed tables and provenance left to the page body.

When a project log exists (a `project-log/` record with effort or experiment
pages), the log executive summary goes into the page's opening "In short"
block, in this chain shape, replacing any activity-shaped summary. It never
introduces a number that the page's tables do not hold.

## The closing marker — one line that stamps the row

The closing block of a turn opens with a marker line. The Stop hook reads
it, stamps the session's work-state from it, and takes the rest of the line
as the nav-row summary, so the chat and the row come from one sentence and
cannot disagree. No separate status call is needed at turn end.

| Marker | State | The line, then the block |
|---|---|---|
| `SITREP: <headline>` | done (blue) | one-line headline, then the chain above |
| `SITREP-QUESTION: <the question>` | blocked (red) | the exact question in one sentence, then the context to answer it cold: what was being done, the options and what follows from each, the default if unanswered, what is irreversible |
| `SITREP-WAITING: <what for>` | waiting (purple) | one sentence, then EVERY armed monitor, background job, subagent and peer request: what it is, what completion looks like, expected duration, the fallback if it never lands, and what happens when it does |

The marker starts a line (bold-wrapping it is fine). The block is the last
thing in the reply. A turn that ends parked (blocked / waiting / done) from a
human prompt and carries no marker is nudged once for the shape it owes; a
long human turn ending green without one is asked once whether it closed an
effort (then it owes `SITREP:`) or was a step in an exchange (then nothing);
a plain answer needs no marker and floors as before. The language rules are
yours to keep: the hook never sends a block back (a send-back can only append
a second block under the first, which is what a reader then sees). The question and waiting
blocks follow the same language rules as the sitrep: plain words, no
identifiers, about 80–150 words. Mid-turn stamps (marking `waiting` the
moment a job is launched) stay as they are; the marker is the turn-end word.

## Before sending: the two checks

1. **Vocabulary scan.** Read the draft as the field colleague. Circle every
   noun the project coined. Replace or define each one.
2. **Chain check.** Cover each paragraph and ask whether the next still
   follows. If a link is "we then ran X", it is activity, not a link; rewrite
   it as what X was for and what it showed.

## Worked example

A tracking project, six efforts in one day, three of them course corrections.
The sitrep that closes the day:

> The tracker follows single molecules through fluorescence movies by sampling
> the space of possible track sets under one exact probabilistic model. Judged
> against eight kinds of simulated movies, it passes none. The most significant
> remaining failure is on the crowded movies, 24 dim and fast molecules made to
> look like real cell data: it reports far too many tracks, typically 32 for 19
> molecules.
>
> Today asked where those extra tracks come from. Three tests settled it. A run
> started from the correct answer stays there. A movie whose molecules never
> approach one another is tracked perfectly. And the extra tracks appear
> exactly where two molecules pass within one resolution width of each other.
> So the cause is neither the model nor the optics: when two molecules overlap,
> the tracker lets one spot explain both, breaks both tracks there, and never
> finds its way back, because no single step of its search can add the missing
> spot and refit both paths at once. A claim made midway, that the model's
> priors reward broken tracks, came from comparing a fitted state with an
> unfitted one and was withdrawn.
>
> Since the search cannot cross that gap, we tried the route that has worked
> before: deterministic clean-up steps run before sampling starts, each kept
> only if it raises the model's own score. A new step carries both molecules
> through an overlap and refits them. On saved runs it removed about a third of
> the extra tracks. On the full benchmark it fired on a third of runs and cut
> the crowded-movie count from 32 to 27 tracks, but the sampling that follows
> undid most of it. The gain, about 6 points on a 100-point track-quality
> score, is below the 8-point noise between two identical runs. The shipped
> defaults are unchanged; the new step is available as an option.
>
> What this means: every molecule is found. What the tracker cannot do is keep
> two identities apart while they overlap, and its sampler walks away from any
> state that does. The binding-detection movies fail for the same reason, so
> this is now the single question in front of the project.
>
> Plan: this needs a decision on how an unresolved overlap is represented in
> the model, not another search move. That decision is yours. The six effort
> pages dated 2026-09-08 in the project log hold the numbers.

Compare with the message it replaced, which opened "The paired ladder is
closed and recorded. Negative. The two new repairs fire on 52 of 144 chains,
add 153 accepted windows, and take the crowded T4 median count from 32 to 27
against a truth of 19 ... the pin stays at v2." Same facts; unreadable to the
person who needed them.
