import React from 'react';
import { AbsoluteFill } from 'remotion';
import { Background } from './scenes/Background';
import { TerminalWindows } from './scenes/TerminalWindows';
import { DatabasePulse } from './scenes/DatabasePulse';
import { DataFlow } from './scenes/DataFlow';
export const ParallelAgentsHero: React.FC = () => {
  return (
    <AbsoluteFill
      style={{
        backgroundColor: '#0D1117',
        overflow: 'hidden',
      }}
    >
      <Background />
      <TerminalWindows />
      <DatabasePulse />
      <DataFlow />
    </AbsoluteFill>
  );
};
