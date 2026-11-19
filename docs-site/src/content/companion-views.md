---
title: Choose your view
---

Self-Reflect's companion shows what it handed Claude this session, the dreamer's "what if...?", and which code changed since your past sessions. It has two views. Both come from the same plugin, `csr-companion`, which `csr-engine setup` installs. Setup does not pick a view. The plugin picks one each time a session starts and each time you type `/csr`: if Claude Code runs inside a herdr pane, you get the herdr pane; if not, you get the panel inside Claude Code.

## Which one should I use

| Where you run `claude` | What you get | How it opens |
|---|---|---|
| A terminal, inside [herdr](https://herdr.dev) | The **herdr pane**: a split to the right of Claude, labelled `CSR`, with the full card set and keys (`i` figure, `b` theme, `q` close) | By itself when a session starts in a project folder. `/csr` opens it again. |
| Inside herdr, in macOS Terminal.app | The herdr pane, with the cards drawn as text, not pictures | Same as above |
| A plain terminal, no herdr | The **in-Claude panel**: a smaller card set inside Claude Code's own window, under the conversation | Type `/csr` |

Use herdr if you want the companion on screen all the time, with the full cards. Use the in-Claude panel if you do not want another tool: it needs nothing beyond setup.

### Requirements

- Both views: `csr-engine setup` (or `csr-engine companion plugin install`), and Claude Code 2.1.296 or later. The 10.1.0 first-run test used Claude Code 2.1.328, started with plain `claude`. Setup sets no environment variable for the plugin.
- The herdr pane: herdr installed and on your `PATH` (the 10.1.0 test used herdr 0.9.3). `HERDR_BIN_PATH` points the plugin at another herdr binary.
- Pictures in the herdr pane: a terminal that draws kitty graphics, such as Ghostty, kitty, WezTerm or Warp. In Terminal.app the pane draws text cards.

## Set up the in-Claude panel (no herdr)

1. Run setup. It installs the plugin after its other steps succeed, under the same consent:
   ```bash
   csr-engine setup
   ```
   The setup output includes `companion: csr-companion 10.1.0 from claude-self-reflect, scope user`.
2. Restart Claude Code, or run `/reload-plugins` in a session that is already open.
3. Open Claude Code in a project you have worked in.
4. Type `/csr` and press Enter.

**What you should see:** Claude Code answers `CSR pane opened (N dreams this week, N changed since your sessions)` and a short note on dreams. Under it, a panel with a `THIS SESSION` bar and cards such as "Dreams are on" and "What CSR can do here". In the 10.1.0 clean-machine test the panel did not open by itself in a plain terminal, so type `/csr` in each session where you want it.

## Set up the herdr pane

1. Install herdr:
   ```bash
   brew install herdr
   ```
2. Run setup, if you have not:
   ```bash
   csr-engine setup
   ```
3. Start herdr in your project folder:
   ```bash
   cd /path/to/your/project
   herdr
   ```
4. In the herdr pane, start Claude Code:
   ```bash
   claude
   ```

**What you should see:** within a few seconds a second pane opens to the right of Claude, titled `CSR`. Its footer reads `i figure  b theme  q close`. In a project with past sessions it shows cards such as "Last time here", "Earlier here" and "Try a question". The pane opens by itself only when the folder is a git repository or a folder Self-Reflect has sessions from. Elsewhere, type `/csr`.

`/csr` in herdr closes any older `CSR` pane in the same tab and opens a fresh one, so the pane always follows the session you typed it in. Its answer reads `CSR pane opened beside this one`.

## Check which view is active

- Type `/csr`. The answer says `CSR pane opened beside this one` in herdr, and `CSR pane opened` for the in-Claude panel.
- In the shell where you start `claude`, run `echo $HERDR_PANE_ID`. A value means the plugin will use the herdr pane. Empty means the in-Claude panel.
- Check that the plugin is installed and on:
  ```bash
  claude plugin list
  ```
  Look for `csr-companion@claude-self-reflect`.
- The herdr pane is a process of its own. `ps -ax | grep "csr-engine companion pane"` lists it while it runs.

## Switch views

You do not run setup again to switch. The plugin decides at each session start and each `/csr`.

**From the in-Claude panel to herdr:**

1. `brew install herdr`
2. Quit Claude Code.
3. Run `herdr` in your project folder, then `claude` inside it.

**From herdr to the in-Claude panel:**

1. Quit Claude Code in herdr. Press `q` in the `CSR` pane to close it.
2. Start `claude` in a plain terminal window.
3. Type `/csr`.

If the plugin cannot open a herdr split (herdr is not on your `PATH`, or a herdr command fails), it opens the in-Claude panel instead.

## Turn it off

- **Close it for now:** press `q` in the herdr pane, or close the in-Claude panel with the `×` at its top right. The herdr pane opens again at the next session start in a project folder.
- **Turn the companion off:** there is no switch for the view alone. Disabling the plugin turns off everything it does in Claude Code, `/csr` included:
  ```bash
  claude plugin disable csr-companion@claude-self-reflect
  ```
  Later runs of `csr-engine setup` keep it disabled. `claude plugin enable csr-companion@claude-self-reflect` turns it back on.
- **Never install it:** `csr-engine setup --no-companion`. To add it later: `csr-engine companion plugin install`.

Search, the hooks, the MCP tools and the daemon do not need the companion. They keep working with it off.

## Picture or text cards

This applies to the herdr pane only. The in-Claude panel is drawn by Claude Code and has no pictures.

- herdr started from any terminal except Terminal.app sends picture cards.
- herdr started from Terminal.app gets text cards, because Terminal.app does not draw kitty graphics. The pane knows the app herdr's server was started from, even when you attach from another terminal.
- `CSR_COMPANION_PIXELS=0` asks for text cards anywhere. `CSR_COMPANION_PIXELS=1` forces pictures on, for example for a herdr server started in Terminal.app that you attach to from Ghostty.

The pane reads the variable, not Claude Code. Set it where herdr's panes start, for example in `~/.zshrc`, then start a new herdr session.

## Troubleshooting

### No panel after setup, plain terminal
Type `/csr`. Without herdr the panel may not open by itself; in the 10.1.0 clean-machine test it did not.

### `/csr` is not a command
The plugin is not loaded. Run `/reload-plugins` or restart Claude Code. If `claude plugin list` does not show `csr-companion@claude-self-reflect`, run:
```bash
csr-engine companion plugin install
```
If it is listed but disabled, run `claude plugin enable csr-companion@claude-self-reflect`.

### In herdr, no `CSR` pane opens
- The folder may be neither a git repository nor a folder Self-Reflect has sessions from. Type `/csr`.
- Check that `herdr` runs from the shell Claude Code was started in. If it does not, the plugin falls back to the in-Claude panel.

### Black boxes, garbled rows or no pictures in the herdr pane
Your terminal does not draw the pictures. Use text cards: set `CSR_COMPANION_PIXELS=0` where herdr's panes start, then start a new herdr session.

### Text cards, but your terminal draws pictures
The herdr server was started from Terminal.app. Start herdr from your picture terminal, or set `CSR_COMPANION_PIXELS=1`.

### "CSR engine missing" or "CSR engine keeps stopping"
The plugin could not start `csr-engine companion serve`. Run `csr-engine setup` again. It rewrites the plugin with the path of the binary you run.

## Other mods in this repository

The `mods/` folder holds other Claude Code mods. They are not the companion, and setup installs none of them.

| Mod | What it is | Install |
|---|---|---|
| `csr-pane` | A TypeScript pane that paints the cards inside Claude Code, opened with `/csr-pane`. Optional. Not part of the 10.1.0 first-run test. | From a clone: `mods/csr-pane/install.sh install`. Undo: `mods/csr-pane/install.sh rollback`. |
| `csr-recall` | A spike: recall as an in-process function hook. | Loaded by hand with `CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1 claude --plugin-dir <path>`. See `mods/csr-recall/README.md`. |
| `csr-decide` | A spike: typed decisions from a local endpoint. | Same flag. See `mods/csr-decide/README.md`. |
