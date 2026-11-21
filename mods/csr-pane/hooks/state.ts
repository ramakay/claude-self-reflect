// Names shared by the files of the mod. The atoms are written in each file that
// uses them: the validator follows a state source only where it is defined.
export const PANE = 'self-reflect'
export const TITLE = 'Self-Reflect'
// The dock width the pane asks for. On a 122-column terminal the default share is 51 and
// leaves the chat 70; 42 leaves it 79. A width the person dragged still wins.
export const DOCK_COLUMNS = 42
export const COMMAND = 'csr-pane'
export const HIDE_KEY = 'band-hidden'
// The engine's text frame is always asked this wide: one row is then one paragraph, and
// Claude Code wraps it to the pane. The rows are more than the main page fills.
export const FRAME_COLS = 160
export const FRAME_ROWS = 60
// The height the pane asks for above the prompt, where the default is a third of the rows.
export const INLINE_ROWS = 16
