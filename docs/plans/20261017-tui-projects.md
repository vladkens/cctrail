Status: draft, first thoughts; not agreed for implementation.

# Projects in the interactive view

Name, merge and group projects from the interactive view instead of editing the config by hand.

## Thoughts

- `/` already filters the list; add marking several rows and a key that merges them into one
  project, and one that renames or moves a project into a group.
- The view writes the result into `~/.config/cctrail/config.toml`, keeping the file's existing
  entries and comments, and reloads.
- Writing the config changes the scope rule in agents.md (cctrail writes only under `~/.cache`).

## Open questions

- Whether the view may write the config at all, or only print the lines to paste.
- How to undo a merge.
