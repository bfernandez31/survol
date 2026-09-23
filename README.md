# survol

*A bird's-eye view of large pull requests.*

survol is a terminal UI for reviewing very large GitLab merge requests (hundreds of
files, often AI-generated). The goal is to understand how the whole thing fits
together and form an architectural opinion without reading every line. It is not
a bug finder.

See [HANDOFF.md](HANDOFF.md) for the vision, decisions and roadmap.

## Status

| Step | | |
|---|---|---|
| 0 | Skeleton, config, `survol-cli doctor` | ✅ |
| 1 | Diff view on a real MR, persistent review state | ✅ |
| 2 | Stack view (LLM grouping) | ✅ (to validate on a real MR) |
| 3 | Graph (tree-sitter) | engine ✅, TUI view ✅ (framework rules in progress) |
| 4 | Neovim integration | ✅ |
| 5 | Questions and GitLab comments | — |

## Install

```sh
cargo install --path crates/survol-tui   # the `survol` binary
cargo install --path crates/survol-cli   # `survol-cli` (JSON engine, doctor)
```

## Configure

`~/.config/survol/config.toml`, overridable per project in `.survol/config.toml`:

```toml
[gitlab]
host = "gitlab.corp.example"      # required, or GITLAB_HOST
# ca_cert = "/path/to/corp-ca.pem" # added to the system trust store (or SURVOL_CA_CERT)

[git]
remote = "origin"

[review]
mechanical_globs = ["**/openapi/generated/**"]  # on top of the built-in lockfile/generated globs

[llm]
# enabled = false                 # never call the LLM (same as --no-llm): group by directory
command = "claude"
group_model = "sonnet"            # model used to group hunks (default: the CLI's default)
# group_effort = "low"            # reasoning effort for grouping (default low: much faster)
# ask_model = "opus"              # model used to answer questions
# config_dir = "~/.claude-work"   # CLAUDE_CONFIG_DIR for the CLI: use another Claude account
# max_prompt_chars = 150000       # bigger reviews are grouped module by module, then merged
# language = "fr"                 # language of titles and summaries (default English; --lang overrides)
```

Project-specific architecture conventions can be written in `.survol/instructions.md`;
they are added to the grouping prompt.

The token comes from `GITLAB_TOKEN`, or from glab's config for that host
(`glab auth login --hostname <host>`). Check everything with:

```sh
survol-cli doctor
```

## Use

```sh
survol              # MR of the current branch
survol 123          # MR !123 of this repo's project
survol https://gitlab.corp.example/group/app/-/merge_requests/123
survol main..feat   # local range, diffed from the merge base
survol --no-llm 123 # never call the LLM: the Stack view groups by directory
survol --lang fr 123 # group titles and summaries in French (a code or a name)
```

The MR head is fetched from `refs/merge-requests/<iid>/head` and checked out in a
dedicated worktree under `.git/survol/worktrees/`, so your working branch is never
touched. Review state lives in `.git/survol/reviews/`.

Three views, `Tab` / `Shift-Tab` (or `1` / `2` / `3`) to switch:

- **Diff**: every file in one stream. `j/k`, `n/N` hunk, `J/K` file, `space` mark
  hunk reviewed, `r` mark file reviewed, `u` next unreviewed, `/` filter files.
- **Stack**: hunks grouped by functional capability, then by layer, in reading
  order, with a short functional summary per group. Grouping runs in the
  background when survol opens (the Diff view is usable meanwhile) and is cached.
  `space` validates the selected group / layer / hunk and moves on, `u` next
  unreviewed group, `J/K` group, `h/l` fold / unfold, `Enter` or `gd` shows the
  hunk in the Diff view, `w` grouping warnings, `R` regroups (new LLM call, asks first).
  The mechanical group (lockfiles, generated code, whitespace, pure renames) comes
  last, folded, and is validated with one `space`. Once the code graph is built,
  groups are reordered along it (a group whose symbols others use comes first).
- **Graph**: the code graph around the changes, built in the background when survol
  opens (tree-sitter, from git objects; cached). `m` switches between:
  - *changed symbols*, by module and file, with their callers and how many of them
    live in files the diff does not touch (impact on untouched code);
  - *symbol view*: a tree "Called by / Calls / Tests", then any other edge kind
    (`uses`, `injects`, `http calls`…), each node with its `file:line`, whether it
    is modified / in the diff / intact, the link's confidence when below 1, and
    `via` for calls through an interface. `l` / `h` expand / collapse a node to walk
    the same relation further (callers of callers…), `Enter` makes a node the new
    root, `Backspace` / `Ctrl-o` goes back. The right pane previews the code at the
    node (changed lines highlighted);
  - *module map*: packages / directories with their dependencies in and out;
    `Enter` lists a module's changed symbols, `x` writes the map as Mermaid to
    `.git/survol/exports/`.

  `/` finds any symbol by name, `e` opens the node's line in the editor (also in
  unchanged files), `gd` shows the symbol's hunks in the Diff view. Test code calling
  a symbol is listed under *Tests*, not *Called by*.

Everywhere: `Ctrl-h` / `Ctrl-l` focus list / content, `B` hide the list, `s` split
view, `e` open in editor, `?` the keys of the current view. In the Diff and Stack
views, `gs` opens the Graph view on the symbol under the cursor.

"Reviewed" is keyed by hunk content: when new commits are pushed, only the hunks
that changed come back as unreviewed.

### Neovim

survol.nvim (Neovim ≥ 0.10) runs survol in a floating terminal that stays
alive while you read code, like lazygit.nvim.

```lua
-- lazy.nvim
{
  dir = "path/to/survol/nvim",
  cmd = { "Survol", "SurvolToggle" },
  keys = { { "<leader>sv", "<cmd>SurvolToggle<cr>", desc = "survol" } },
  opts = {
    -- cmd = "survol", args = { "--no-llm" },
    -- width = 0.95, height = 0.92, border = "rounded",
    -- open_mode = "edit",   -- "edit" (window survol was opened from), "tab", "split", "vsplit"
    -- toggle_key = "<C-g>", -- also hides the float from inside the TUI
  },
}
```

| Command | |
|---|---|
| `:Survol [mr\|url\|base..head] [--no-llm] [--lang fr]` | Shows the running review, or starts one (a different target restarts it). Completes flags and branches. |
| `:SurvolToggle` | Hides / shows the float; survol keeps running. |
| `:SurvolBack` | Back to the review, where you left it. |
| `:SurvolClose` | Stops survol. |
| `:checkhealth survol` | Binary, version, RPC socket. |

The loop: `e` in the TUI calls `require("survol").open_file(path, line, col)` in
the parent Neovim (`nvim --server $NVIM --remote-expr`); the float is hidden,
not closed, and the file opens at the line with your LSP and keymaps.
`:SurvolBack` (or your toggle key) shows the same TUI at the same position.
Without the plugin loaded, survol falls back to `:tabedit +line`. Outside
Neovim, `e` suspends the TUI and runs `$VISUAL` / `$EDITOR`.

## Develop

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
survol-cli diff main..HEAD | jq '.files | length'
survol-cli group main..HEAD | jq '.groups[] | {title, hunks: (.hunk_ids | length)}'
survol-cli graph main..HEAD | jq '.symbols[] | {name, callers: [.callers[] | .name]}'
survol-cli graph main..HEAD --symbol OwnerService.find   # one symbol, changed or not
survol-cli graph main..HEAD --modules                    # package / directory dependencies
survol-cli graph main..HEAD --mermaid                    # the same, as a Mermaid flowchart
nvim --headless -u NONE -i NONE -c "luafile nvim/tests/survol_spec.lua"   # survol.nvim tests
```

Groupings are cached in `.git/survol/cache/<head>/groups.json`; `--no-cache` recomputes,
`--no-llm` groups by directory without calling the LLM, `--lang <LANG>` sets the
language of titles and summaries.
`SURVOL_LLM_LOG=<dir>` keeps every prompt and raw LLM answer for debugging.

`survol-cli graph` indexes the head (and the base version of changed files) with
tree-sitter — Java, Kotlin, TypeScript/TSX, JavaScript — and prints the changed
symbols with their callers, callees and tests. Each link has a `confidence`
(1.0 certain, lower when several candidates match or only the name agrees) and
says whether the other file is part of the diff. Parsed files are cached by blob
in `.git/survol/cache/index/`, the graph in `.git/survol/cache/<head>/graph.json`
(`--no-cache` rebuilds it). The index queries live in `crates/survol-core/queries/`.
