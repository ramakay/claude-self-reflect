import React from 'react';
import { useCurrentFrame, interpolate, Easing, random } from 'remotion';
import { colors, timing } from '../styles/colors';

interface ParticleProps {
  id: number;
  originX: number;
  originY: number;
}

const Particle: React.FC<ParticleProps> = ({ id, originX, originY }) => {
  const frame = useCurrentFrame();

  const crashStart = timing.crashMoment;
  const progress = interpolate(
    frame,
    [crashStart, crashStart + 60],
    [0, 1],
    { extrapolateLeft: 'clamp', extrapolateRight: 'clamp', easing: Easing.out(Easing.quad) }
  );

  // Fade for loop
  const fadeForLoop = interpolate(
    frame,
    [timing.fadeToGray, timing.totalFrames],
    [1, 0],
    { extrapolateLeft: 'clamp', extrapolateRight: 'clamp' }
  );

  const angle = random(`angle-${id}`) * Math.PI * 2;
  const speed = 20 + random(`speed-${id}`) * 50;
  const size = 4 + random(`size-${id}`) * 8;

  const x = originX + Math.cos(angle) * speed * progress;
  const y = originY + Math.sin(angle) * speed * progress + progress * progress * 25;

  const opacity = interpolate(progress, [0, 0.1, 1], [0, 0.5, 0]) * fadeForLoop;

  const particleColor = [colors.coral, colors.accent, colors.blue][id % 3];

  if (frame < crashStart) return null;

  return (
    <g transform={`translate(${x}, ${y})`} opacity={opacity}>
      <rect
        x={-size / 2}
        y={-size / 2}
        width={size}
        height={size}
        fill="none"
        stroke={particleColor}
        strokeWidth={1.5}
        transform={`rotate(${id * 15})`}
      />
    </g>
  );
};

export const Crash: React.FC = () => {
  const frame = useCurrentFrame();

  // Match database position
  const originX = 900;
  const originY = 380;

  // Fade for loop
  const fadeForLoop = interpolate(
    frame,
    [timing.fadeToGray, timing.totalFrames],
    [1, 0],
    { extrapolateLeft: 'clamp', extrapolateRight: 'clamp' }
  );

  // Warning icon animation
  const warningOpacity = interpolate(
    frame,
    [timing.crashMoment, timing.crashMoment + 15, timing.fadeToGray],
    [0, 1, 1],
    { extrapolateLeft: 'clamp', extrapolateRight: 'clamp' }
  ) * fadeForLoop;

  const warningScale = interpolate(
    frame,
    [timing.crashMoment, timing.crashMoment + 20],
    [0.5, 1],
    { extrapolateLeft: 'clamp', extrapolateRight: 'clamp', easing: Easing.out(Easing.back(2)) }
  );

  const particles = Array.from({ length: 15 }, (_, i) => i);

  return (
    <svg
      style={{
        position: 'absolute',
        width: '100%',
        height: '100%',
        pointerEvents: 'none',
      }}
    >
      {/* Particles */}
      {particles.map((id) => (
        <Particle key={id} id={id} originX={originX} originY={originY} />
      ))}

      {/* Warning triangle */}
      <g
        transform={`translate(${originX + 80}, ${originY - 120}) scale(${warningScale})`}
        opacity={warningOpacity}
      >
        <path
          d="M 0 -28 L 26 22 L -26 22 Z"
          fill="none"
          stroke={colors.coral}
          strokeWidth={2.5}
          strokeLinejoin="round"
        />
        <line x1="0" y1="-12" x2="0" y2="6" stroke={colors.coral} strokeWidth={3} strokeLinecap="round" />
        <circle cx="0" cy="14" r={3} fill={colors.coral} />
      </g>
    </svg>
  );
};
