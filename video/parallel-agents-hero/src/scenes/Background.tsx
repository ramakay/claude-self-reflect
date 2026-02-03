import React from 'react';
import { useCurrentFrame, interpolate, Easing } from 'remotion';
import { colors, timing } from '../styles/colors';

export const Background: React.FC = () => {
  const frame = useCurrentFrame();

  // Very subtle shake near crash - eases in gently
  const shakeIntensity = interpolate(
    frame,
    [timing.conflictStart + 20, timing.crashMoment, timing.crashMoment + 10],
    [0, 1, 0],
    { extrapolateLeft: 'clamp', extrapolateRight: 'clamp', easing: Easing.in(Easing.quad) }
  );

  const shakeX = Math.sin(frame * 1.2) * shakeIntensity;
  const shakeY = Math.cos(frame * 1.5) * shakeIntensity * 0.6;

  return (
    <div
      style={{
        position: 'absolute',
        width: '100%',
        height: '100%',
        backgroundColor: '#FAFAFA',
        transform: `translate(${shakeX}px, ${shakeY}px)`,
        overflow: 'hidden',
      }}
    >
      {/* Subtle grid dots */}
      <svg
        style={{
          position: 'absolute',
          width: '100%',
          height: '100%',
          opacity: 0.12,
        }}
      >
        <defs>
          <pattern id="dots" width="40" height="40" patternUnits="userSpaceOnUse">
            <circle cx="20" cy="20" r="1" fill={colors.lineLight} />
          </pattern>
        </defs>
        <rect width="100%" height="100%" fill="url(#dots)" />
      </svg>
    </div>
  );
};
