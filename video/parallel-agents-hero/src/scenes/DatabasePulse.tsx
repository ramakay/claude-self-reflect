import React from 'react';
import { useCurrentFrame, useVideoConfig, interpolate, Easing, Img, staticFile } from 'remotion';
import { timing } from '../styles/colors';

export const DatabasePulse: React.FC = () => {
  const frame = useCurrentFrame();
  const { width, height } = useVideoConfig();

  // Detect vertical aspect ratio
  const isVertical = height > width;

  // Position and scale for vertical (centered between left and right stacks)
  const centerX = isVertical ? 540 : 900;
  const centerY = isVertical ? 1350 : 380;
  const baseScale = isVertical ? 2.0 : 1;

  // Fade in
  const fadeIn = interpolate(
    frame,
    [30, 80],
    [0, 1],
    { extrapolateLeft: 'clamp', extrapolateRight: 'clamp', easing: Easing.out(Easing.cubic) }
  );

  // Float in from right
  const floatX = interpolate(
    frame,
    [30, 80],
    [40, 0],
    { extrapolateLeft: 'clamp', extrapolateRight: 'clamp', easing: Easing.out(Easing.cubic) }
  );

  // Gray to color transition (using CSS filter)
  const colorAmount = interpolate(
    frame,
    [timing.colorizeStart, timing.colorizeEnd],
    [0, 1],
    { extrapolateLeft: 'clamp', extrapolateRight: 'clamp', easing: Easing.inOut(Easing.quad) }
  );

  // Heat level for red shift
  const heatLevel = interpolate(
    frame,
    [timing.dataFlowStart, timing.conflictStart, timing.crashMoment],
    [0, 0.3, 1],
    { extrapolateLeft: 'clamp', extrapolateRight: 'clamp' }
  );

  // Fade back to gray at end for loop
  const fadeToGray = interpolate(
    frame,
    [timing.fadeToGray, timing.totalFrames],
    [1, 0],
    { extrapolateLeft: 'clamp', extrapolateRight: 'clamp' }
  );

  // Pulse effect
  const pulseSpeed = 0.08 + heatLevel * 0.1;
  const pulse = Math.sin(frame * pulseSpeed) * 0.5 + 0.5;

  // Gentle shake - eases in
  const shakeIntensity = interpolate(
    frame,
    [timing.conflictStart + 30, timing.crashMoment],
    [0, 3],
    { extrapolateLeft: 'clamp', extrapolateRight: 'clamp', easing: Easing.in(Easing.cubic) }
  );
  const shakeX = Math.sin(frame * 1.2) * shakeIntensity * fadeToGray;
  const shakeY = Math.cos(frame * 1.5) * shakeIntensity * 0.5 * fadeToGray;

  // Filter for color effects
  // Start gray (grayscale), then colorize, then shift to red/orange
  const grayscale = 1 - colorAmount * fadeToGray;
  const hueRotate = heatLevel * fadeToGray * -30;
  const saturate = 1 + heatLevel * fadeToGray * 0.6;
  const brightness = 1 - heatLevel * fadeToGray * 0.1;

  const pulseScale = 1 + pulse * 0.02 * heatLevel * fadeToGray;
  const totalScale = baseScale * pulseScale;

  return (
    <div
      style={{
        position: 'absolute',
        left: centerX - 80 * baseScale + floatX + shakeX,
        top: centerY - 100 * baseScale + shakeY,
        opacity: fadeIn,
        transform: `scale(${totalScale})`,
        transformOrigin: 'center center',
      }}
    >
      {/* Glow effect behind icon */}
      <div
        style={{
          position: 'absolute',
          left: '50%',
          top: '50%',
          width: 200 + pulse * 30 * heatLevel,
          height: 180 + pulse * 20 * heatLevel,
          transform: 'translate(-50%, -50%)',
          background: `radial-gradient(ellipse, ${
            heatLevel < 0.5
              ? `rgba(150, 150, 150, ${0.1 + pulse * 0.05})`
              : `rgba(255, 120, 100, ${0.15 + pulse * 0.1})`
          } 0%, transparent 70%)`,
          borderRadius: '50%',
          filter: 'blur(20px)',
          opacity: fadeToGray,
        }}
      />

      {/* The database icon */}
      <Img
        src={staticFile('pv-128.png')}
        style={{
          width: 160,
          height: 196,
          position: 'relative',
          zIndex: 1,
          filter: `grayscale(${grayscale}) hue-rotate(${hueRotate}deg) saturate(${saturate}) brightness(${brightness})`,
        }}
      />
    </div>
  );
};
