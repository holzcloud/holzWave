// The command palette's matcher: a case-insensitive subsequence match with a score, so "spl" finds "Split pane
// right" and a match that starts words or runs unbroken ranks above one that is scattered. Pure: no DOM, no state,
// node imports it for the unit tests.

/** Points for every matched character. */
const MATCH = 16;
/** Extra points for a character that directly follows the previous matched one. */
const CONSECUTIVE = 24;
/** Extra points for a character that starts a word (the first character, after a separator, a camel hump). */
const WORD_START = 20;
/** A match of the very same case as the query (a typed capital means a capital). */
const SAME_CASE = 1;
/** Points taken for every character skipped between two matched ones. */
const GAP = 3;
/** Points taken for every character before the first match, at most `LEAD_CAP` of them. */
const LEAD = 1;
const LEAD_CAP = 6;
/** A shorter title wins a tie of everything else: the one the person most likely means. */
const LENGTH = 0.01;

/** The most characters of a text that are looked at; a longer title is matched on its start. */
const MAX_TEXT = 256;

const NONE = Number.NEGATIVE_INFINITY;

/** Characters that are not part of a word: what follows one starts a word. */
const isSeparator = (ch: string): boolean => !/[\p{L}\p{N}]/u.test(ch);

/** Whether the character at `at` starts a word of `chars`. */
function startsWord(chars: readonly string[], at: number): boolean {
  if (at === 0) return true;
  const before = chars[at - 1] as string;
  const here = chars[at] as string;
  if (isSeparator(before)) return !isSeparator(here);
  // a lower-case letter followed by an upper-case one is a new word in camelCase
  return before !== before.toUpperCase() && here !== here.toLowerCase();
}

/**
 * How well `query` matches `text`: null when `query` is not a subsequence of `text` (ignoring case), otherwise a
 * number where a larger one is a better match. An empty query matches everything with the score 0. The score is
 * the best alignment of the query's characters in the text: every match earns points, a match right after the
 * previous one and a match that starts a word earn more, a gap and a late start cost a little, and a shorter text
 * wins a tie. The scores are only comparable for one query.
 */
export function fuzzyScore(query: string, text: string): number | null {
  const q = Array.from(query);
  if (q.length === 0) return 0;
  const t = Array.from(text).slice(0, MAX_TEXT);
  if (q.length > t.length) return null;
  const qLower = q.map((c) => c.toLowerCase());
  const tLower = t.map((c) => c.toLowerCase());

  // prev[j]: the best score with the previous query character matched at text position j (NONE: impossible).
  let prev: number[] = new Array<number>(t.length).fill(NONE);
  for (let i = 0; i < q.length; i++) {
    const cur: number[] = new Array<number>(t.length).fill(NONE);
    // run[j]: the best prev[k] with k <= j, less the gap penalty for the characters between k and j.
    let run = NONE;
    let runBefore = NONE; // run at j - 2: the best a non-consecutive match at j can build on
    let last = NONE; // run at j - 1
    for (let j = 0; j < t.length; j++) {
      runBefore = last;
      last = run;
      if (i > 0) run = Math.max(run - GAP, prev[j] as number);
      if (tLower[j] !== qLower[i]) continue;
      let base = MATCH + (startsWord(t, j) ? WORD_START : 0) + (t[j] === q[i] ? SAME_CASE : 0);
      if (i === 0) {
        base -= Math.min(j, LEAD_CAP) * LEAD;
        cur[j] = base;
        continue;
      }
      let best = NONE;
      if (j >= 1 && (prev[j - 1] as number) > NONE) best = (prev[j - 1] as number) + CONSECUTIVE;
      if (j >= 2 && runBefore > NONE) best = Math.max(best, runBefore - GAP);
      if (best > NONE) cur[j] = best + base;
    }
    prev = cur;
  }
  let best = NONE;
  for (const s of prev) if (s > best) best = s;
  return best === NONE ? null : best - t.length * LENGTH;
}

/** One item of a ranked list: the item, its score and its place in the list that was ranked. */
export interface Ranked<T> {
  item: T;
  score: number;
  /** The position of the item in the input, which orders equal scores. */
  index: number;
}

/**
 * The items whose text matches `query`, best first; equal scores keep the order of `items` (an empty query keeps
 * all of them in their order). `text` names what is matched for an item.
 */
export function rank<T>(query: string, items: readonly T[], text: (item: T) => string): Ranked<T>[] {
  const out: Ranked<T>[] = [];
  items.forEach((item, index) => {
    const score = fuzzyScore(query, text(item));
    if (score !== null) out.push({ item, score, index });
  });
  return out.sort((a, b) => b.score - a.score || a.index - b.index);
}
