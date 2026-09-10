# Pane repaint diagnosis

Pane reads and metadata writes can repaint a pane that the user is watching.
The confirmed costly path is:

- `herdr pane read <id> --source recent` (and `recent-unwrapped`) — rebuilds the
  pane's wrapped scrollback. Measured at **4.45s per call** against 0.006s for
  `--source visible`, and it repaints the pane: the agent's TUI redraws its whole
  frame, which the user sees as the terminal scrolling up and snapping back to
  the bottom. **One read, one scroll** — confirmed 1:1 by burst-reading a live
  pane while the user watched (2 `recent` reads → 2 scrolls; 13 `visible` reads
  → none).

`herdr pane report-metadata <id>` also carries repaint risk when tokens change,
so suppress unchanged writes.

Use `--source visible` (or `detection`; both return the current screen). The
prompt is on screen at the moment `idle->working` fires, which is exactly when
the topic changes. Later in the turn it may have scrolled off — then extraction
returns `None` and the caller must keep the topic it already published.

| `--source` | cost | repaints |
|---|---|---|
| `visible` | 0.006s | no |
| `detection` | 0.004s | no |
| `recent` | 4.452s | **yes** |
| `recent-unwrapped` | 4.448s | **yes** |

`pane get` offsets and before/after content hashes do not detect the transient
repaint: the TUI redraws and returns to the same content. Verify suspected
regressions with a short call burst while a person watches the pane.

## Debugging a "the panes are scrolling" report

Bisect in this order, with one observed reproduction per step:

1. `herdr plugin disable herdr-agent-quota` **and** remove the `statusLine`
   entry from `~/.claude/settings.json`, then **restart the agent pane**.
   `herdr plugin disable` alone is not enough — Claude Code runs the statusLine
   command itself, independent of Herdr, and reads the setting at startup.
2. Restore the statusLine only. Scrolls → the statusLine hook is at fault.
3. Re-enable the plugin, then remove event hooks from `herdr-plugin.toml` one
   at a time, reloading with `herdr plugin disable && herdr plugin enable`
   (needed to re-read the manifest; `herdr server reload-config` does not).

To capture an event payload, temporarily point a hook at
`sh -c "printf '%s' \"$HERDR_PLUGIN_EVENT_JSON\" > /tmp/ev.json; exec <real command>"`
so the plugin keeps working while you collect the shape. Do not use a polling
pane-read probe; the probe itself can cause the repaint.
