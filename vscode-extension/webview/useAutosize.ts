import * as React from 'react';

/** Auto-grow a textarea up to a max height (default 140px). Resizes
 *  on every value change. Returns a ref to attach to the textarea. */
export function useAutosize(
  value: string,
  maxHeight = 140,
): React.RefObject<HTMLTextAreaElement> {
  const ref = React.useRef<HTMLTextAreaElement>(null!);
  React.useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = Math.min(el.scrollHeight, maxHeight) + 'px';
  }, [value, maxHeight]);
  return ref;
}
