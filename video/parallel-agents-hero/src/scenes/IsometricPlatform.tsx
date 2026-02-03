import React from 'react';
import { colors } from '../styles/colors';

interface PlatformProps {
  x: number;
  y: number;
  width: number;
  depth: number;
  height?: number;
  opacity?: number;
}

// Isometric platform component
export const IsometricPlatform: React.FC<PlatformProps> = ({
  x,
  y,
  width,
  depth,
  height = 8,
  opacity = 1,
}) => {
  // Isometric conversion factors
  const isoX = (px: number, py: number) => px - py;
  const isoY = (px: number, py: number) => (px + py) / 2;

  // Platform corners (in isometric space)
  const topFace = [
    { x: isoX(0, 0), y: isoY(0, 0) },
    { x: isoX(width, 0), y: isoY(width, 0) },
    { x: isoX(width, depth), y: isoY(width, depth) },
    { x: isoX(0, depth), y: isoY(0, depth) },
  ];

  const rightFace = [
    { x: isoX(width, 0), y: isoY(width, 0) },
    { x: isoX(width, depth), y: isoY(width, depth) },
    { x: isoX(width, depth), y: isoY(width, depth) + height },
    { x: isoX(width, 0), y: isoY(width, 0) + height },
  ];

  const leftFace = [
    { x: isoX(0, depth), y: isoY(0, depth) },
    { x: isoX(width, depth), y: isoY(width, depth) },
    { x: isoX(width, depth), y: isoY(width, depth) + height },
    { x: isoX(0, depth), y: isoY(0, depth) + height },
  ];

  const toPath = (points: { x: number; y: number }[]) =>
    points.map((p, i) => `${i === 0 ? 'M' : 'L'} ${p.x} ${p.y}`).join(' ') + ' Z';

  return (
    <g transform={`translate(${x}, ${y})`} opacity={opacity}>
      {/* Left side face */}
      <path
        d={toPath(leftFace)}
        fill={colors.platformSide}
        stroke={colors.platformEdge}
        strokeWidth="1"
      />
      {/* Right side face */}
      <path
        d={toPath(rightFace)}
        fill={colors.platformSide}
        stroke={colors.platformEdge}
        strokeWidth="1"
      />
      {/* Top face */}
      <path
        d={toPath(topFace)}
        fill={colors.platformTop}
        stroke={colors.platformEdge}
        strokeWidth="1"
      />
    </g>
  );
};
