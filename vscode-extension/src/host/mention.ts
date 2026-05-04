// @-mention detection for "actively spawn a Claude turn".
//
// Different from the Rust `cc-connect-core::hook_format::mentions_self`
// (which fires the `for-you` HOOK directive on bare `@<self>` too).
// Reason: the hook is *passive context injection* on a Claude turn that
// is already going to run; this function gates the *active spawn* of a
// new query. Bare `@<self>` is an address to the HUMAN, not the AI —
// peers chatting "yo @yjj seen this?" should not auto-summon yjj's
// Claude. To explicitly address the AI peer, use `@<self>-cc`. To
// address every AI in the room at once, use a broadcast token.

const BROADCAST_TOKENS = ['cc', 'claude', 'all', 'here'] as const;

/** Returns true iff `body` is explicitly addressed to the local AI:
 *  `@<myNick>-cc` (the AI mirror form) or any broadcast token
 *  (`@cc` / `@claude` / `@all` / `@here`). Word-boundary semantics —
 *  `@yjj-ccc hi` does NOT register. Bare `@<myNick>` is intentionally
 *  NOT matched (that's an address to the human). */
export function shouldWakeClaude(body: string, myNick: string): boolean {
  if (!myNick || myNick.length === 0) return false;
  const lower = body.toLowerCase();
  if (matchAtToken(lower, `${myNick.toLowerCase()}-cc`)) return true;
  for (const tok of BROADCAST_TOKENS) {
    if (matchAtToken(lower, tok)) return true;
  }
  return false;
}

function matchAtToken(lower: string, target: string): boolean {
  const needle = `@${target}`;
  let from = 0;
  for (;;) {
    const i = lower.indexOf(needle, from);
    if (i < 0) return false;
    const next = lower.charAt(i + needle.length);
    if (next === '' || !isNickCont(next)) return true;
    from = i + 1;
  }
}

function isNickCont(c: string): boolean {
  return /[A-Za-z0-9_-]/.test(c);
}
