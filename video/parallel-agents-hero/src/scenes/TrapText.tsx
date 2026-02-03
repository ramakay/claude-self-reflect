import React from 'react';
import { useCurrentFrame, interpolate, Easing } from 'remotion';
import { timing } from '../styles/colors';

export const TrapText: React.FC = () => {
  const frame = useCurrentFrame();

  // Position - bottom center of composition
  const centerX = 640;
  const centerY = 560;

  // Animation phases
  const fadeIn = interpolate(
    frame,
    [60, 120],
    [0, 1],
    { extrapolateLeft: 'clamp', extrapolateRight: 'clamp', easing: Easing.out(Easing.cubic) }
  );

  // Gray to color
  const colorAmount = interpolate(
    frame,
    [timing.colorizeStart, timing.colorizeEnd],
    [0, 1],
    { extrapolateLeft: 'clamp', extrapolateRight: 'clamp', easing: Easing.inOut(Easing.quad) }
  );

  // Heat/danger level
  const heatLevel = interpolate(
    frame,
    [timing.conflictStart, timing.crashMoment],
    [0, 1],
    { extrapolateLeft: 'clamp', extrapolateRight: 'clamp', easing: Easing.in(Easing.quad) }
  );

  // Fade for loop
  const fadeForLoop = interpolate(
    frame,
    [timing.fadeToGray, timing.totalFrames],
    [1, 0],
    { extrapolateLeft: 'clamp', extrapolateRight: 'clamp' }
  );

  // Float up animation
  const floatY = interpolate(
    frame,
    [60, 120],
    [20, 0],
    { extrapolateLeft: 'clamp', extrapolateRight: 'clamp', easing: Easing.out(Easing.cubic) }
  );

  // Layer configuration - Breaking Bad style but muted
  const layerCount = 6;
  const layerOffsetX = 3;
  const layerOffsetY = 3;

  // Muted color layers (bottom to top)
  const layerColors = [
    '#3d4f6f', // Muted dark blue (bottom)
    '#5a7a9a', // Muted steel blue
    '#7a9a8a', // Muted teal
    '#9a8a7a', // Muted tan
    '#bab0a0', // Muted beige
    '#e8e4e0', // Off-white (top)
  ];

  // Gray versions for start
  const grayColors = [
    '#3a3a3a',
    '#4a4a4a',
    '#5a5a5a',
    '#6a6a6a',
    '#8a8a8a',
    '#c0c0c0',
  ];

  // Muted danger colors (warm shift, not bright red)
  const dangerColors = [
    '#5a3a3a',
    '#7a4a4a',
    '#9a5a5a',
    '#ba7a6a',
    '#da9a8a',
    '#f0e0d8',
  ];

  // Get interpolated color for a layer
  const getLayerColor = (layerIndex: number) => {
    const gray = grayColors[layerIndex];
    const color = layerColors[layerIndex];
    const danger = dangerColors[layerIndex];

    const parseHex = (hex: string) => ({
      r: parseInt(hex.slice(1, 3), 16),
      g: parseInt(hex.slice(3, 5), 16),
      b: parseInt(hex.slice(5, 7), 16),
    });

    const grayRGB = parseHex(gray);
    const colorRGB = parseHex(color);
    const dangerRGB = parseHex(danger);

    const effectiveColor = colorAmount * fadeForLoop;
    const effectiveDanger = heatLevel * fadeForLoop;

    let r = grayRGB.r + (colorRGB.r - grayRGB.r) * effectiveColor;
    let g = grayRGB.g + (colorRGB.g - grayRGB.g) * effectiveColor;
    let b = grayRGB.b + (colorRGB.b - grayRGB.b) * effectiveColor;

    r = r + (dangerRGB.r - r) * effectiveDanger;
    g = g + (dangerRGB.g - g) * effectiveDanger;
    b = b + (dangerRGB.b - b) * effectiveDanger;

    return `rgb(${Math.round(r)}, ${Math.round(g)}, ${Math.round(b)})`;
  };

  // Subtle shake during crash
  const shakeIntensity = interpolate(
    frame,
    [timing.crashMoment - 30, timing.crashMoment],
    [0, 2],
    { extrapolateLeft: 'clamp', extrapolateRight: 'clamp', easing: Easing.in(Easing.quad) }
  ) * fadeForLoop;

  const shakeX = Math.sin(frame * 1.5) * shakeIntensity;
  const shakeY = Math.cos(frame * 1.8) * shakeIntensity * 0.5;

  return (
    <div
      style={{
        position: 'absolute',
        left: centerX + shakeX,
        top: centerY + floatY + shakeY,
        opacity: fadeIn,
        transform: 'translate(-50%, -50%)',
        textAlign: 'center',
        perspective: '800px',
      }}
    >
      <div
        style={{
          transform: 'rotateX(70deg) skewX(-2deg)',
          transformStyle: 'preserve-3d',
        }}
      >
      {/* Render layers from bottom to top */}
      {Array.from({ length: layerCount }).map((_, layerIndex) => {
        const reverseIndex = layerCount - 1 - layerIndex;
        const offsetX = reverseIndex * layerOffsetX;
        const offsetY = reverseIndex * layerOffsetY;
        const color = getLayerColor(layerIndex);
        const isTopLayer = layerIndex === layerCount - 1;

        // Stagger layer appearance
        const layerDelay = layerIndex * 3;
        const layerOpacity = interpolate(
          frame,
          [60 + layerDelay, 90 + layerDelay],
          [0, 1],
          { extrapolateLeft: 'clamp', extrapolateRight: 'clamp' }
        );

        return (
          <div
            key={layerIndex}
            style={{
              position: 'absolute',
              left: 0,
              top: 0,
              transform: `translate3d(${offsetX}px, ${offsetY}px, ${-reverseIndex * 2}px)`,
              opacity: layerOpacity,
              fontFamily: "'Inter', 'Helvetica Neue', Arial, sans-serif",
              fontWeight: 800,
              letterSpacing: '-0.02em',
              lineHeight: 0.9,
              color: color,
              WebkitTextStroke: isTopLayer ? '1px #2a2a2a' : 'none',
              textShadow: isTopLayer ? 'none' : `1px 1px 0 ${color}`,
            }}
          >
            <div style={{ fontSize: 28, marginBottom: 2 }}>PARALLEL AGENT</div>
            <div style={{ fontSize: 56 }}>TRAP</div>
          </div>
        );
      })}
      </div>
    </div>
  );
};
