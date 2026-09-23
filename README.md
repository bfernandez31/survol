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
| 3 | Graph (tree-sitter) | — |
| 4 | Neovim integration | minimal plugin |
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

Two views, `Tab` / `Shift-Tab` (or `1` / `2`) to switch:

- **Diff**: every file in one stream. `j/k`, `n/N` hunk, `J/K` file, `space` mark
  hunk reviewed, `r` mark file reviewed, `u` next unreviewed, `/` filter files.
- **Stack**: hunks grouped by functional capability, then by layer, in reading
  order, with a short functional summary per group. Grouping runs in the
  background when survol opens (the Diff view is usable meanwhile) and is cached.
  `space` validates the selected group / layer / hunk and moves on, `u` next
  unreviewed group, `J/K` group, `h/l` fold / unfold, `Enter` or `gd` shows the
  hunk in the Diff view, `w` grouping warnings, `R` regroups (new LLM call, asks first).
  The mechanical group (lockfiles, generated code, whitespace, pure renames) comes
  last, folded, and is validated with one `space`.

Everywhere: `Ctrl-h` / `Ctrl-l` focus list / content, `B` hide the list, `s` split
view, `e` open in editor, `?` all keys.

"Reviewed" is keyed by hunk content: when new commits are pushed, only the hunks
that changed come back as unreviewed.

### Neovim

```lua
-- lazy.nvim
{ dir = "path/to/survol/nvim", config = function() require("survol").setup() end }
```

`:Survol [mr]` opens survol in a floating terminal; `e` opens files in that Neovim.
Outside Neovim, `e` runs `$VISUAL` / `$EDITOR`.

## Develop

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
survol-cli diff main..HEAD | jq '.files | length'
survol-cli group main..HEAD | jq '.groups[] | {title, hunks: (.hunk_ids | length)}'
```

Groupings are cached in `.git/survol/cache/<head>/groups.json`; `--no-cache` recomputes,
`--no-llm` groups by directory without calling the LLM, `--lang <LANG>` sets the
language of titles and summaries.
`SURVOL_LLM_LOG=<dir>` keeps every prompt and raw LLM answer for debugging.
