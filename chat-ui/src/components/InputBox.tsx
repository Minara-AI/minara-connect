import React from "react";
import { Box, Text } from "ink";

export interface InputBoxProps {
  value: string;
  cursorVisible: boolean;
}

/** Single-line input box. We don't use ink-text-input because we need
 *  the @-mention popup integrated into key handling — App.tsx owns the
 *  global useInput hook and writes back into `value`. */
export function InputBox({ value, cursorVisible }: InputBoxProps) {
  return (
    <Box borderStyle="single" borderColor="gray" paddingX={1}>
      <Text>
        <Text color="cyan">› </Text>
        {value}
        {cursorVisible ? <Text color="white" backgroundColor="white">{" "}</Text> : null}
      </Text>
    </Box>
  );
}
