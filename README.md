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
| 2 | Stack view (LLM grouping) | — |
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
command = "claude"
```

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
```

The MR head is fetched from `refs/merge-requests/<iid>/head` and checked out in a
dedicated worktree under `.git/survol/worktrees/`, so your working branch is never
touched. Review state lives in `.git/survol/reviews/`.

Press `?` in the UI for all keys. The essentials: `j/k`, `n/N` hunk, `J/K` file,
`space` mark hunk reviewed, `r` mark file reviewed, `u` next unreviewed, `s` split
view, `/` filter files, `e` open in editor.

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
```
