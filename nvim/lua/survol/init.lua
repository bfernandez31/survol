-- survol.nvim: survol in a persistent floating terminal.
--
-- The survol job keeps running while you read code: opening a file from the
-- TUI hides the float (the job and its screen stay alive) and `:Survol` /
-- `:SurvolToggle` shows the same TUI again, where you left it.
--
-- The TUI opens files through `open_file`, called over RPC:
--   nvim --server $NVIM --remote-expr "luaeval(...require'survol'.open_file...)"
local M = {}

local defaults = {
  -- survol binary.
  cmd = "survol",
  -- Arguments always passed before the target (e.g. { "--no-llm" }).
  args = {},
  -- Float size: a fraction of the editor (<= 1) or a number of cells.
  width = 0.95,
  height = 0.92,
  border = "rounded",
  -- Where files opened from survol go: "edit" (the window survol was
  -- opened from; "hide" is an alias), "tab", "split" or "vsplit".
  open_mode = "edit",
  -- Optional key toggling the float (normal mode, and terminal mode inside
  -- the float), e.g. "<leader>sv". No default.
  toggle_key = nil,
}

M.config = vim.deepcopy(defaults)

-- The running survol: { buf, job, win, key, prev_win }.
local state = {}

local function job_alive()
  return state.job ~= nil and vim.fn.jobwait({ state.job }, 0)[1] == -1
end

local function float_valid()
  return state.win ~= nil and vim.api.nvim_win_is_valid(state.win)
end

local function size(v, total)
  if v <= 1 then
    return math.max(1, math.floor(total * v))
  end
  return math.min(total, math.floor(v))
end

local function float_config()
  local cols, lines = vim.o.columns, vim.o.lines - vim.o.cmdheight
  local width, height = size(M.config.width, cols), size(M.config.height, lines)
  return {
    relative = "editor",
    width = width,
    height = height,
    col = math.floor((cols - width) / 2),
    row = math.floor((lines - height) / 2),
    style = "minimal",
    border = M.config.border,
  }
end

-- Opens the float on `buf`, remembering the window to open files in.
local function open_float(buf)
  local cur = vim.api.nvim_get_current_win()
  if vim.api.nvim_win_get_config(cur).relative == "" then
    state.prev_win = cur
  end
  state.win = vim.api.nvim_open_win(buf, true, float_config())
  vim.wo[state.win].winhighlight = "NormalFloat:Normal"
end

--- True while the survol float is on screen.
function M.is_visible()
  return float_valid()
end

--- True while a survol job is running (shown or hidden).
function M.is_running()
  return job_alive()
end

--- Shows the running survol (same job, same screen). Returns false if none.
function M.show()
  if not job_alive() or not vim.api.nvim_buf_is_valid(state.buf) then
    return false
  end
  if not float_valid() then
    open_float(state.buf)
  else
    vim.api.nvim_set_current_win(state.win)
  end
  vim.cmd.startinsert()
  return true
end

--- Hides the float; survol keeps running.
function M.hide()
  if float_valid() then
    -- nvim_win_hide keeps the terminal buffer (bufhidden=hide).
    vim.api.nvim_win_hide(state.win)
  end
  state.win = nil
end

function M.toggle()
  if float_valid() then
    M.hide()
  elseif not M.show() then
    M.open()
  end
end

--- Stops survol and closes its float.
function M.close()
  if job_alive() then
    vim.fn.jobstop(state.job)
  end
  M.hide()
  if state.buf and vim.api.nvim_buf_is_valid(state.buf) then
    vim.api.nvim_buf_delete(state.buf, { force = true })
  end
  state = {}
end

local function normalize(target)
  return vim.trim((target or ""):gsub("%s+", " "))
end

--- `:Survol [target]`: shows the running survol, or starts one when none is
--- running or `target` differs from the running one.
function M.open(target)
  local key = normalize(target)
  if job_alive() and (key == "" or key == state.key) then
    M.show()
    return
  end
  M.close()

  local cmd = { M.config.cmd }
  vim.list_extend(cmd, M.config.args or {})
  if key ~= "" then
    vim.list_extend(cmd, vim.split(key, " ", { trimempty = true }))
  end
  if vim.fn.executable(cmd[1]) == 0 then
    vim.notify("survol: `" .. cmd[1] .. "` not found (:checkhealth survol)", vim.log.levels.ERROR)
    return
  end

  local buf = vim.api.nvim_create_buf(false, true)
  vim.bo[buf].bufhidden = "hide"
  state = { buf = buf, key = key }
  -- jobstart(term=true) runs in the current buffer, sized by its window.
  open_float(buf)
  -- Neovim 0.10 has termopen(); 0.11 deprecates it for jobstart(term=true).
  local start = vim.fn.has("nvim-0.11") == 1 and vim.fn.jobstart or vim.fn.termopen
  local job = start(cmd, {
    term = true,
    cwd = vim.fn.getcwd(),
    on_exit = function(id)
      vim.schedule(function()
        if state.job ~= id then
          return
        end
        M.hide()
        if vim.api.nvim_buf_is_valid(buf) then
          vim.api.nvim_buf_delete(buf, { force = true })
        end
        state = {}
      end)
    end,
  })
  if job <= 0 then
    M.close()
    vim.notify("survol: cannot start " .. cmd[1], vim.log.levels.ERROR)
    return
  end
  state.job = job
  if M.config.toggle_key then
    vim.keymap.set("t", M.config.toggle_key, M.hide, { buffer = buf, desc = "Hide survol" })
  end
  vim.cmd.startinsert()
end

-- A normal (non-floating) window to open files in: the one survol was
-- opened from, else the first one of the current tab.
local function target_win()
  if state.prev_win and vim.api.nvim_win_is_valid(state.prev_win) then
    local tab = vim.api.nvim_win_get_tabpage(state.prev_win)
    if tab == vim.api.nvim_get_current_tabpage() then
      return state.prev_win
    end
  end
  for _, w in ipairs(vim.api.nvim_tabpage_list_wins(0)) do
    if vim.api.nvim_win_get_config(w).relative == "" then
      return w
    end
  end
end

local function place_cursor(line, col)
  local last = vim.api.nvim_buf_line_count(0)
  line = math.max(1, math.min(tonumber(line) or 1, last))
  col = math.max(0, (tonumber(col) or 1) - 1)
  pcall(vim.api.nvim_win_set_cursor, 0, { line, col })
  vim.cmd("normal! zvzz")
end

local function do_open(path, line, col, mode)
  M.hide()
  vim.cmd.stopinsert()
  local win = target_win()
  if win then
    vim.api.nvim_set_current_win(win)
  end
  local fname = vim.fn.fnameescape(path)
  -- A window of this tab already showing the file: just move there.
  local bufnr = vim.fn.bufnr(path)
  local shown = bufnr ~= -1 and vim.fn.win_findbuf(bufnr) or {}
  for _, w in ipairs(shown) do
    if vim.api.nvim_win_get_tabpage(w) == vim.api.nvim_get_current_tabpage()
      and vim.api.nvim_win_get_config(w).relative == "" then
      vim.api.nvim_set_current_win(w)
      return place_cursor(line, col)
    end
  end
  if mode == "tab" then
    vim.cmd("tabedit " .. fname)
  elseif mode == "split" then
    vim.cmd("split " .. fname)
  elseif mode == "vsplit" then
    vim.cmd("vsplit " .. fname)
  else
    vim.cmd("edit " .. fname)
  end
  place_cursor(line, col)
end

--- Opens `path` at `line` / `col` (1-based; col 0 = start of line) and hides
--- the survol float. Called by the TUI over RPC: returns "ok" or
--- "error: <reason>" and does the window work on the next tick, so the call
--- returns at once whatever mode Neovim is in.
function M.open_file(path, line, col, mode)
  if type(path) ~= "string" or path == "" then
    return "error: no path"
  end
  if vim.fn.filereadable(path) == 0 then
    return "error: not readable: " .. path
  end
  mode = mode or M.config.open_mode
  if mode == "hide" then
    mode = "edit"
  end
  if not vim.tbl_contains({ "edit", "tab", "split", "vsplit" }, mode) then
    return "error: unknown open_mode " .. tostring(mode)
  end
  vim.schedule(function()
    local ok, err = pcall(do_open, path, line, col, mode)
    if not ok then
      vim.notify("survol: " .. tostring(err), vim.log.levels.ERROR)
    end
  end)
  return "ok"
end

local flags = { "--no-llm", "--no-lsp", "--lang", "--repo", "-C", "--help", "--version" }

--- Completion for `:Survol`: flags, `--lang` values, and branches for ranges.
function M.complete(arglead, cmdline)
  local words = vim.split(cmdline, "%s+")
  local prev = words[#words - 1]
  local out = {}
  if prev == "--lang" then
    out = { "en", "fr", "de", "es", "it", "pt", "nl" }
  elseif prev == "--repo" or prev == "-C" then
    return vim.fn.getcompletion(arglead, "dir")
  elseif arglead:sub(1, 1) == "-" then
    out = flags
  else
    local prefix, rest = arglead:match("^(.-%.%.)(.*)$")
    local branches = vim.fn.systemlist({ "git", "branch", "-a", "--format=%(refname:short)" })
    if vim.v.shell_error ~= 0 then
      branches = {}
    end
    for _, b in ipairs(branches) do
      table.insert(out, (prefix or "") .. b)
    end
    arglead = prefix and (prefix .. rest) or arglead
  end
  return vim.tbl_filter(function(c)
    return vim.startswith(c, arglead)
  end, out)
end

function M.define_commands()
  vim.api.nvim_create_user_command("Survol", function(a)
    M.open(a.args)
  end, {
    nargs = "*",
    complete = M.complete,
    desc = "survol: show the running review, or review [mr|url|base..head] [flags]",
  })
  vim.api.nvim_create_user_command("SurvolToggle", M.toggle, { desc = "survol: show / hide" })
  vim.api.nvim_create_user_command("SurvolBack", function()
    if not M.show() then
      vim.notify("survol is not running", vim.log.levels.WARN)
    end
  end, { desc = "survol: back to the review" })
  vim.api.nvim_create_user_command("SurvolClose", M.close, { desc = "survol: stop" })
end

function M.setup(opts)
  M.config = vim.tbl_deep_extend("force", vim.deepcopy(defaults), opts or {})
  M.define_commands()
  if M.config.toggle_key then
    vim.keymap.set("n", M.config.toggle_key, M.toggle, { desc = "Toggle survol" })
  end
  local group = vim.api.nvim_create_augroup("survol", { clear = true })
  vim.api.nvim_create_autocmd("VimResized", {
    group = group,
    callback = function()
      if float_valid() then
        vim.api.nvim_win_set_config(state.win, float_config())
      end
    end,
  })
end

-- For tests.
M._state = function()
  return state
end

return M
