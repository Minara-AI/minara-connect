import React from "react";
import { Box, Text } from "ink";

export interface MentionPopupProps {
  candidates: readonly string[];
  selectedIdx: number;
}

/** @-mention completion popup. Renders inline above the input box;
 *  parent shows/hides based on `currentAtToken` + non-empty candidates. */
export function MentionPopup({ candidates, selectedIdx }: MentionPopupProps) {
  if (candidates.length === 0) return null;
  return (
    <Box
      flexDirection="column"
      borderStyle="single"
      borderColor="cyan"
      paddingX={1}
    >
      {candidates.map((nick, i) => {
        const sel = i === selectedIdx;
        return (
          <Text key={`${nick}-${i}`} color={sel ? "black" : "white"} backgroundColor={sel ? "cyan" : undefined}>
            {sel ? "▶ " : "  "}@{nick}
          </Text>
        );
      })}
      <Text dimColor>↑↓ select · Tab/Enter accept · Esc cancel</Text>
    </Box>
  );
}
