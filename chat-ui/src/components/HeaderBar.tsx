import React from "react";
import { Box, Text } from "ink";

export interface HeaderBarProps {
  topicShort: string;
  selfNick: string | null;
  daemonAlive: boolean;
}

/** Top strip: room id + you-are-X + connection status. The ticket goes
 *  in a separate line/overlay so it doesn't fight the chat scrollback for
 *  width on narrow panes. */
export function HeaderBar({ topicShort, selfNick, daemonAlive }: HeaderBarProps) {
  return (
    <Box flexDirection="row" justifyContent="space-between" paddingX={1}>
      <Text bold>
        <Text color="cyan">Room </Text>
        <Text color="white">{topicShort}</Text>
      </Text>
      <Text dimColor>
        you = <Text color="green">{selfNick ?? "(no nick)"}</Text>
        {"  "}
        <Text color={daemonAlive ? "green" : "red"}>
          {daemonAlive ? "● daemon up" : "○ daemon down"}
        </Text>
      </Text>
    </Box>
  );
}
