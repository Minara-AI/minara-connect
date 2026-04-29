// @-mention completion logic. Direct port of
// crates/cc-connect-tui/src/mention.rs — keep them in sync.
//
// Pure functions; the popup state machine lives in the InputBox
// component.

/** Special broadcast tokens. Hook (`hook_format::mentions_self`) treats
 * these as "everyone listening" — keep this list in sync with
 * `cc-connect-core/src/hook_format.rs::mentions_self`. */
const BROADCAST_TOKENS = ["cc", "claude", "all", "here"] as const;

/** If `input` ends with an in-progress `@<prefix>` (no whitespace after
 *  the last `@`), return the prefix without the `@`. Returns `null` if
 *  the cursor is not in an at-token. The prefix may be `""` when the
 *  user has just typed `@`. */
export function currentAtToken(input: string): string | null {
  const at = input.lastIndexOf("@");
  if (at < 0) return null;
  const after = input.slice(at + 1);
  if (/\s/.test(after)) return null;
  // Avoid email-like patterns ("foo@bar"): if the char before `@` is
  // alphanumeric, it's not a mention.
  if (at > 0) {
    const prev = input[at - 1]!;
    if (/[A-Za-z0-9]/.test(prev)) return null;
  }
  return after;
}

/** Filter `recent` (most-recent-first) by case-insensitive `startsWith`,
 *  excluding `selfNick`, then synthesise the user's own AI peer
 *  (`<selfNick>-cc`, never lands in `recent` because the listener
 *  filters own-pubkey messages) and finally append the broadcast tokens. */
export function mentionCandidates(
  recent: readonly string[],
  prefix: string,
  selfNick: string | null,
): string[] {
  const lower = prefix.toLowerCase();
  const selfLower = selfNick && selfNick.length > 0 ? selfNick.toLowerCase() : null;

  const out: string[] = [];
  const seen = new Set<string>();

  // Synthetic own-AI peer up front (matches the Rust ordering).
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

/** Replace the trailing `@<prefix>` in `input` with `@<full> ` (with a
 *  trailing space so the user can keep typing the body). */
export function completeAt(input: string, fullNick: string): string {
  const at = input.lastIndexOf("@");
  if (at < 0) return input;
  return input.slice(0, at + 1) + fullNick + " ";
}

/** Body-content scan for @-mentions of the receiving user. Mirrors
 *  `cc_connect::chat_session::line_mentions_me` and
 *  `cc-connect-core::hook_format::mentions_self`. */
export function bodyMentionsSelf(body: string, selfNick: string | null): boolean {
  const lower = body.toLowerCase();
  if (
    lower.includes("@cc") ||
    lower.includes("@claude") ||
    lower.includes("@all") ||
    lower.includes("@here")
  ) {
    return true;
  }
  if (selfNick && selfNick.length > 0) {
    const token = `@${selfNick.toLowerCase()}`;
    if (lower.includes(token)) return true;
  }
  return false;
}
