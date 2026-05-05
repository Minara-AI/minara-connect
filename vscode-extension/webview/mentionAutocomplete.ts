// @-mention completion logic — port of chat-ui/src/mention.ts (and
// in turn of crates/cc-connect-tui/src/mention.rs). Pure functions;
// the popup state machine lives in Chat.tsx.

const BROADCAST_TOKENS = ['cc', 'claude', 'all', 'here'] as const;

/** If `input` ends with an in-progress `@<prefix>` (no whitespace
 *  after the last `@`), return the prefix without the `@`. Returns
 *  null if the cursor is not in an at-token. The prefix may be `""`
 *  when the user has just typed `@`. */
export function currentAtToken(input: string): string | null {
  const at = input.lastIndexOf('@');
  if (at < 0) return null;
  const after = input.slice(at + 1);
  if (/\s/.test(after)) return null;
  // Avoid email-like patterns ("foo@bar"): if the char before `@` is
  // alphanumeric, it's not a mention.
  if (at > 0) {
    const prev = input[at - 1];
    if (prev && /[A-Za-z0-9]/.test(prev)) return null;
  }
  return after;
}

/** Filter `recent` (most-recent-first) by case-insensitive
 *  `startsWith`, excluding `selfNick`, then synthesise the user's own
 *  AI peer (`<selfNick>-cc`) and finally append the broadcast tokens. */
export function mentionCandidates(
  recent: readonly string[],
  prefix: string,
  selfNick: string | null,
): string[] {
  const lower = prefix.toLowerCase();
  const selfLower =
    selfNick && selfNick.length > 0 ? selfNick.toLowerCase() : null;

  const out: string[] = [];
  const seen = new Set<string>();

  // Synthetic own-AI peer up front.
  if (selfNick && selfNick.length > 0) {
    const ownAi = `${selfNick}-cc`;
    const ownLower = ownAi.toLowerCase();
    if (ownLower.startsWith(lower) && !seen.has(ownLower)) {
      out.push(ownAi);
      seen.add(ownLower);
    }
  }

  for (const nick of recent) {
    const nLower = nick.toLowerCase();
    if (selfLower !== null && nLower === selfLower) continue;
    if (!nLower.startsWith(lower)) continue;
    if (!seen.has(nLower)) {
      out.push(nick);
      seen.add(nLower);
    }
  }

  for (const tok of BROADCAST_TOKENS) {
    if (!tok.startsWith(lower)) continue;
    const lt = tok.toLowerCase();
    if (!seen.has(lt)) {
      out.push(tok);
      seen.add(lt);
    }
  }
  return out;
}

/** Replace the trailing `@<prefix>` in `input` with `@<full> ` (with
 *  a trailing space so the user can keep typing the body). */
export function completeAt(input: string, fullNick: string): string {
  const at = input.lastIndexOf('@');
  if (at < 0) return input;
  return input.slice(0, at + 1) + fullNick + ' ';
}
