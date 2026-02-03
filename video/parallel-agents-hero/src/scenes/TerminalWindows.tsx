import React from 'react';
import { useCurrentFrame, useVideoConfig, interpolate, Easing } from 'remotion';
import { colors, timing } from '../styles/colors';

// Compact mini terminal for stacking
const MiniTerminal: React.FC<{
  x: number;
  y: number;
  index: number;
  opacity: number;
  colorAmount: number; // 0 = gray, 1 = full color
  redAmount: number; // 0 = normal color, 1 = fully red
  baseColor: string;
}> = ({ x, y, index, opacity, colorAmount, redAmount, baseColor }) => {
  const cardWidth = 70;
  const cardHeight = 40;

  // Gray to color interpolation
  const grayColor = colors.gray;

  // Calculate display color: gray -> baseColor -> red
  let displayColor: string;
  if (redAmount > 0) {
    // Transition to red
    displayColor = `rgb(${Math.round(232 + (255 - 232) * redAmount)}, ${Math.round(146 * (1 - redAmount) + 80 * redAmount)}, ${Math.round(124 * (1 - redAmount) + 80 * redAmount)})`;
  } else if (colorAmount < 1) {
    // Gray to color transition
    displayColor = grayColor;
  } else {
    displayColor = baseColor;
  }

  // Status indicator
  const statusColor = redAmount > 0.5 ? '#FF4444' : (colorAmount < 0.5 ? grayColor : baseColor);

  // Traffic light colors (gray when not colorized)
  const redLight = colorAmount > 0.3 ? '#FF6B6B' : '#CCCCCC';
  const yellowLight = colorAmount > 0.5 ? (redAmount > 0.3 ? '#FF6B6B' : '#FFD93D') : '#CCCCCC';
  const greenLight = colorAmount > 0.7 ? displayColor : '#CCCCCC';

  return (
    <g transform={`translate(${x}, ${y})`} opacity={opacity}>
      {/* Card shadow */}
      <rect
        x={3}
        y={3}
        width={cardWidth}
        height={cardHeight}
        rx={6}
        fill={redAmount > 0.5 ? '#FFB0B0' : colors.lineLight}
        opacity={0.15 + redAmount * 0.1}
      />

      {/* Card body */}
      <rect
        width={cardWidth}
        height={cardHeight}
        rx={6}
        fill="white"
        stroke={redAmount > 0.3 ? `rgba(255, 100, 100, ${0.5 + redAmount * 0.5})` : colors.platformEdge}
        strokeWidth={1.5}
      />

      {/* Header bar */}
      <rect
        width={cardWidth}
        height={14}
        rx={6}
        fill={redAmount > 0.5 ? '#FFE5E5' : colors.platformTop}
      />
      <rect
        y={10}
        width={cardWidth}
        height={4}
        fill={redAmount > 0.5 ? '#FFE5E5' : colors.platformTop}
      />

      {/* Traffic lights */}
      <circle cx={10} cy={7} r={2.5} fill={redLight} opacity={0.9} />
      <circle cx={18} cy={7} r={2.5} fill={yellowLight} opacity={0.9} />
      <circle cx={26} cy={7} r={2.5} fill={greenLight} opacity={0.9} />

      {/* Code lines */}
      <g transform="translate(6, 18)">
        <rect width={28} height={3} rx={1.5} fill={displayColor} opacity={0.5 + colorAmount * 0.3} />
        <rect y={6} width={45} height={3} rx={1.5} fill={redAmount > 0.5 ? '#FFCCCC' : colors.lineLight} />
        <rect y={12} width={35} height={3} rx={1.5} fill={redAmount > 0.5 ? '#FFCCCC' : colors.lineLight} />
      </g>

      {/* Status dot */}
      <circle
        cx={cardWidth - 10}
        cy={cardHeight - 10}
        r={4}
        fill={statusColor}
        opacity={0.5 + colorAmount * 0.3 + redAmount * 0.2}
      />
    </g>
  );
};

export const TerminalWindows: React.FC = () => {
  const frame = useCurrentFrame();
  const { width, height } = useVideoConfig();

  // Detect vertical aspect ratio
  const isVertical = height > width;

  // Scale and position for vertical (symmetric around center 540)
  const scale = isVertical ? 1.6 : 1;
  const baseXLeft = isVertical ? 60 : 200;
  const baseXRight = isVertical ? 908 : 200; // Right side stack for vertical
  const baseY = isVertical ? 300 : 140;
  const terminalHeight = isVertical ? 85 : 48;
  const visibleTerminals = 8;

  // Color cycle
  const getColor = (i: number) => {
    const colorCycle = [colors.accent, colors.blue, colors.coral];
    return colorCycle[i % 3];
  };

  // Easing for different groups
  const getDelay = (i: number) => {
    if (i < 3) return i * 8;
    if (i < 6) return 24 + (i - 3) * 12;
    return 60 + (i - 6) * 15;
  };

  return (
    <svg
      style={{
        position: 'absolute',
        width: '100%',
        height: '100%',
      }}
    >
      {Array.from({ length: visibleTerminals }).map((_, i) => {
        const delay = getDelay(i);

        // Fade in
        const fadeIn = interpolate(
          frame,
          [delay, delay + 40],
          [0, 1],
          { extrapolateLeft: 'clamp', extrapolateRight: 'clamp', easing: Easing.out(Easing.cubic) }
        );

        // Gray to color transition
        const colorAmount = interpolate(
          frame,
          [timing.colorizeStart + i * 8, timing.colorizeEnd + i * 8],
          [0, 1],
          { extrapolateLeft: 'clamp', extrapolateRight: 'clamp', easing: Easing.inOut(Easing.quad) }
        );

        // Red propagation
        const redDelay = i * 8;
        const redAmount = interpolate(
          frame,
          [timing.conflictStart + redDelay, timing.conflictStart + redDelay + 60],
          [0, 1],
          { extrapolateLeft: 'clamp', extrapolateRight: 'clamp', easing: Easing.in(Easing.quad) }
        );

        // Fade back to gray at end for loop
        const fadeToGray = interpolate(
          frame,
          [timing.fadeToGray, timing.totalFrames],
          [1, 0],
          { extrapolateLeft: 'clamp', extrapolateRight: 'clamp' }
        );

        // Float in
        const floatX = interpolate(
          frame,
          [delay, delay + 40],
          [-30, 0],
          { extrapolateLeft: 'clamp', extrapolateRight: 'clamp', easing: Easing.out(Easing.cubic) }
        );

        // Staggered vertical position
        const yPos = baseY + i * terminalHeight;
        const xPosLeft = baseXLeft + floatX + i * (isVertical ? 8 : 5);
        const xPosRight = baseXRight - floatX - i * (isVertical ? 8 : 5); // Mirror on right

        if (isVertical) {
          // Render both left and right stacks for vertical
          return (
            <g key={i}>
              {/* Left stack */}
              <g transform={`scale(${scale})`}>
                <MiniTerminal
                  x={xPosLeft / scale}
                  y={yPos / scale}
                  index={i}
                  baseColor={getColor(i)}
                  opacity={fadeIn}
                  colorAmount={colorAmount * fadeToGray}
                  redAmount={redAmount * fadeToGray}
                />
              </g>
              {/* Right stack */}
              <g transform={`scale(${scale})`}>
                <MiniTerminal
                  x={xPosRight / scale}
                  y={yPos / scale}
                  index={i + 8}
                  baseColor={getColor(i + 1)}
                  opacity={fadeIn}
                  colorAmount={colorAmount * fadeToGray}
                  redAmount={redAmount * fadeToGray}
                />
              </g>
            </g>
          );
        }

        return (
          <MiniTerminal
            key={i}
            x={xPosLeft}
            y={yPos}
            index={i}
            baseColor={getColor(i)}
            opacity={fadeIn}
            colorAmount={colorAmount * fadeToGray}
            redAmount={redAmount * fadeToGray}
          />
        );
      })}
    </svg>
  );
};
