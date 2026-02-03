import React from 'react';
import { useCurrentFrame, useVideoConfig, interpolate, Easing } from 'remotion';
import { colors, timing } from '../styles/colors';

interface FlowLineProps {
  startX: number;
  startY: number;
  endX: number;
  endY: number;
  delay: number;
  color: string;
  index: number;
}

const FlowLine: React.FC<FlowLineProps> = ({ startX, startY, endX, endY, delay, color, index }) => {
  const frame = useCurrentFrame();

  const flowStart = timing.dataFlowStart + delay;

  // Line draw progress
  const drawProgress = interpolate(
    frame,
    [flowStart, flowStart + 40],
    [0, 1],
    { extrapolateLeft: 'clamp', extrapolateRight: 'clamp', easing: Easing.out(Easing.quad) }
  );

  // Gray to color transition
  const colorAmount = interpolate(
    frame,
    [timing.colorizeStart, timing.colorizeEnd],
    [0, 1],
    { extrapolateLeft: 'clamp', extrapolateRight: 'clamp' }
  );

  // Animated dash offset for flowing effect
  const dashOffset = -frame * 1.5;

  // Multiple particles along line
  const particleCount = 2;
  const particles = Array.from({ length: particleCount }, (_, i) => {
    const offset = i / particleCount;
    const t = ((frame - flowStart) * 0.03 + offset) % 1;
    return t;
  });

  // Smooth bezier curve
  const dx = endX - startX;
  const dy = endY - startY;
  const ctrl1X = startX + dx * 0.4;
  const ctrl1Y = startY;
  const ctrl2X = startX + dx * 0.6;
  const ctrl2Y = endY;

  // Cubic bezier point calculation
  const bezierPoint = (t: number) => {
    const t2 = t * t;
    const t3 = t2 * t;
    const mt = 1 - t;
    const mt2 = mt * mt;
    const mt3 = mt2 * mt;
    const x = mt3 * startX + 3 * mt2 * t * ctrl1X + 3 * mt * t2 * ctrl2X + t3 * endX;
    const y = mt3 * startY + 3 * mt2 * t * ctrl1Y + 3 * mt * t2 * ctrl2Y + t3 * endY;
    return { x, y };
  };

  // Fade out at end for loop
  const fadeOut = interpolate(
    frame,
    [timing.fadeToGray, timing.totalFrames],
    [1, 0],
    { extrapolateLeft: 'clamp', extrapolateRight: 'clamp' }
  );

  // Display color (gray -> color)
  const displayColor = colorAmount > 0.5 ? color : colors.gray;
  const lineOpacity = drawProgress * fadeOut * 0.4;
  const showParticles = frame >= flowStart && drawProgress > 0.1;

  // Path for smooth cubic bezier
  const pathD = `M ${startX} ${startY} C ${ctrl1X} ${ctrl1Y}, ${ctrl2X} ${ctrl2Y}, ${endX} ${endY}`;

  return (
    <>
      {/* Connection line with animated dash */}
      <path
        d={pathD}
        fill="none"
        stroke={displayColor}
        strokeWidth={2}
        strokeDasharray="6 4"
        strokeDashoffset={dashOffset}
        opacity={lineOpacity}
        strokeLinecap="round"
      />

      {/* Subtle glow */}
      <path
        d={pathD}
        fill="none"
        stroke={displayColor}
        strokeWidth={6}
        opacity={lineOpacity * 0.1}
        strokeLinecap="round"
      />

      {/* Flowing particles */}
      {showParticles && particles.map((t, i) => {
        if (t < 0.05 || t > 0.95) return null;
        const pos = bezierPoint(t);
        const particleOpacity = Math.sin(t * Math.PI) * fadeOut * 0.7 * colorAmount;
        return (
          <g key={i}>
            <circle
              cx={pos.x}
              cy={pos.y}
              r={4}
              fill={displayColor}
              opacity={particleOpacity}
            />
            <circle
              cx={pos.x}
              cy={pos.y}
              r={8}
              fill={displayColor}
              opacity={particleOpacity * 0.25}
            />
          </g>
        );
      })}
    </>
  );
};

export const DataFlow: React.FC = () => {
  const { width, height } = useVideoConfig();

  // Detect vertical aspect ratio
  const isVertical = height > width;

  // Database position for vertical (centered) - must match DatabasePulse.tsx
  const dbX = isVertical ? 540 : 900;
  const dbY = isVertical ? 1350 : 380;

  // Terminal positions for vertical (symmetric around center 540)
  const baseXLeft = isVertical ? 60 : 200;
  const baseXRight = isVertical ? 908 : 200;
  const baseY = isVertical ? 300 : 140;
  const terminalHeight = isVertical ? 85 : 48;
  const scale = isVertical ? 1.6 : 1;
  const cardWidth = isVertical ? 70 * scale : 70;
  const visibleFlows = 8;

  const getColor = (i: number) => {
    const colorCycle = [colors.accent, colors.blue, colors.coral];
    return colorCycle[i % 3];
  };

  return (
    <svg
      style={{
        position: 'absolute',
        width: '100%',
        height: '100%',
        pointerEvents: 'none',
      }}
    >
      {/* Glow filter */}
      <defs>
        <filter id="lineGlow" x="-50%" y="-50%" width="200%" height="200%">
          <feGaussianBlur stdDeviation="2" result="blur" />
          <feMerge>
            <feMergeNode in="blur" />
            <feMergeNode in="SourceGraphic" />
          </feMerge>
        </filter>
      </defs>

      {Array.from({ length: visibleFlows }).map((_, i) => {
        // Left side - start from right edge of each terminal card
        const xOffsetLeft = isVertical ? i * 8 : i * 5;
        const startXLeft = baseXLeft + xOffsetLeft + cardWidth;
        const startY = baseY + i * terminalHeight + (isVertical ? 34 : 20);

        // Right side - start from left edge of each terminal card (for vertical)
        const xOffsetRight = isVertical ? i * 8 : 0;
        const startXRight = baseXRight - xOffsetRight;

        return (
          <g key={i}>
            {/* Left side flow */}
            <FlowLine
              startX={startXLeft}
              startY={startY}
              endX={dbX}
              endY={dbY}
              delay={i * 6}
              color={getColor(i)}
              index={i}
            />
            {/* Right side flow (only for vertical) */}
            {isVertical && (
              <FlowLine
                startX={startXRight}
                startY={startY}
                endX={dbX}
                endY={dbY}
                delay={i * 6 + 3}
                color={getColor(i + 1)}
                index={i + 8}
              />
            )}
          </g>
        );
      })}
    </svg>
  );
};
