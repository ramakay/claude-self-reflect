// Clean isometric illustration palette
export const colors = {
  // Base
  background: '#FFFFFF',
  white: '#FFFFFF',

  // Grays for structure
  platformTop: '#F5F5F5',
  platformSide: '#E8E8E8',
  platformEdge: '#CCCCCC',

  // Line work
  line: '#333333',
  lineMedium: '#666666',
  lineLight: '#999999',

  // Initial gray state
  gray: '#AAAAAA',
  grayLight: '#CCCCCC',

  // Accent (lime green like reference)
  accent: '#C4D600',
  accentDark: '#9FB000',

  // Secondary accents
  blue: '#4A90D9',
  coral: '#E8927C',

  text: '#333333',
  textMuted: '#888888',
};

// Isometric helpers
export const iso = {
  // Standard isometric angles
  angle: 30,
  // Transform for isometric view
  transform: 'rotateX(60deg) rotateZ(-45deg)',
  // Scale factors
  xScale: 0.866, // cos(30)
  yScale: 0.5,   // sin(30)
};

// Timing (in frames at 30fps) - 3x longer for smoother animation
export const timing = {
  fps: 30,
  // Gray to color transition
  colorizeStart: 60,
  colorizeEnd: 150,
  // Terminal animations
  terminalSlideIn: 0,
  terminalTyping: 90,
  // Data flow
  dataFlowStart: 180,
  // Conflict and crash
  conflictStart: 360,
  crashMoment: 450,
  crashHold: 540,
  // Loop point - fade back to gray
  fadeToGray: 560,
  totalFrames: 630,
};
