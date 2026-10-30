Part of fe-ui: see ../CLAUDE.md.

Mouse routing focuses the agent pane or flowed text preview before starting its selection. A left press outside
each pane clears that owner's selection; motion extends an active drag to the visible edge and release ends it.
The preview uses ShapedPreview's buffer positions and rect, and is selectable only while flowed text is shown.
