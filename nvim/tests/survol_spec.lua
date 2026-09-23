-- Headless tests for survol.nvim. From the repository root:
--   nvim --headless -u NONE -i NONE -c "luafile nvim/tests/survol_spec.lua"
-- A fake long-running job stands in for survol. Exits 1 if any test fails.

vim.opt.rtp:prepend(vim.fn.getcwd() .. "/nvim")
vim.o.swapfile = false
if vim.v.servername == "" then
  vim.fn.serverstart()
end

local survol = require("survol")
local failures = 0

local function test(name, fn)
  local ok, err = pcall(fn)
  if ok then
    io.stdout:write("ok   " .. name .. "\n")
  else
    failures = failures + 1
    io.stdout:write("FAIL " .. name .. ": " .. tostring(err) .. "\n")
  end
  -- Back to a single empty window between tests.
  pcall(survol.close)
  vim.cmd("silent! tabonly | silent! only | enew!")
end

local function eq(a, b, msg)
  if not vim.deep_equal(a, b) then
    error((msg or "") .. " expected " .. vim.inspect(b) .. ", got " .. vim.inspect(a), 2)
  end
end

-- Runs pending vim.schedule callbacks.
local function flush()
  vim.wait(50, function()
    return false
  end)
end

local tmp = vim.fn.tempname()
vim.fn.mkdir(tmp .. "/my dir", "p")
tmp = vim.fn.resolve(tmp)
local function fixture(name)
  local path = tmp .. "/" .. name
  local lines = {}
  for i = 1, 100 do
    lines[i] = ("line %d  text"):format(i)
  end
  vim.fn.writefile(lines, path)
  return path
end
local plain = fixture("plain.txt")
local spaced = fixture("my dir/it's a \"file\" [1].txt")

-- A stand-in for survol: a long-running terminal job.
survol.setup({ cmd = "sh", args = { "-c", "echo survol-fake; exec sleep 600", "sh" } })

local function current_path()
  return vim.fs.normalize(vim.api.nvim_buf_get_name(0))
end

test("open starts a job in a float", function()
  survol.open()
  local st = survol._state()
  assert(survol.is_running(), "job running")
  assert(survol.is_visible(), "float shown")
  eq(vim.api.nvim_win_get_config(0).relative, "editor")
  eq(vim.bo[st.buf].buftype, "terminal")
end)

test("toggle hides and shows the same job and buffer", function()
  survol.open()
  local st = survol._state()
  local job, buf = st.job, st.buf
  survol.toggle()
  assert(not survol.is_visible(), "hidden")
  assert(survol.is_running(), "still running")
  assert(vim.api.nvim_buf_is_valid(buf), "buffer kept")
  survol.toggle()
  assert(survol.is_visible(), "shown again")
  eq(survol._state().job, job, "same job")
  eq(vim.api.nvim_win_get_buf(0), buf, "same buffer")
  -- :Survol without a target shows the running one too.
  survol.hide()
  vim.cmd("Survol")
  eq(survol._state().job, job, ":Survol reuses the job")
  vim.cmd("SurvolToggle")
  vim.cmd("SurvolBack")
  eq(vim.api.nvim_win_get_buf(0), buf, ":SurvolBack shows it")
end)

test("a different target restarts the job", function()
  survol.open("main..a")
  local job = survol._state().job
  survol.open("main..a")
  eq(survol._state().job, job, "same target: same job")
  survol.open("main..b")
  assert(survol._state().job ~= job, "new target: new job")
  eq(vim.fn.jobwait({ job }, 1000)[1] ~= -1, true, "old job stopped")
end)

test("open_file hides the float and opens at line/col (edit)", function()
  local origin = vim.api.nvim_get_current_win()
  survol.open()
  eq(survol.open_file(plain, 42, 6), "ok")
  flush()
  assert(not survol.is_visible(), "float hidden")
  assert(survol.is_running(), "job alive")
  eq(vim.api.nvim_get_current_win(), origin, "opened in the previous window")
  eq(current_path(), vim.fs.normalize(plain))
  eq(vim.api.nvim_win_get_cursor(0), { 42, 5 })
  eq(vim.fn.mode(), "n")
end)

test("open_file modes: tab, split, vsplit", function()
  survol.open()
  survol.open_file(plain, 10, 0, "tab")
  flush()
  eq(vim.fn.tabpagenr("$"), 2, "new tab")
  eq(vim.api.nvim_win_get_cursor(0), { 10, 0 })
  vim.cmd("tabonly | enew")

  survol.show()
  survol.open_file(plain, 11, 0, "split")
  flush()
  eq(#vim.api.nvim_tabpage_list_wins(0), 2, "split")
  eq(vim.api.nvim_win_get_cursor(0)[1], 11)
  vim.cmd("only | enew")

  survol.show()
  survol.open_file(plain, 12, 0, "vsplit")
  flush()
  eq(#vim.api.nvim_tabpage_list_wins(0), 2, "vsplit")
  eq(vim.fn.winlayout()[1], "row")
  eq(vim.api.nvim_win_get_cursor(0)[1], 12)
end)

test("open_file reuses a window already showing the file", function()
  vim.cmd("edit " .. vim.fn.fnameescape(plain))
  vim.cmd("vsplit | enew")
  survol.open()
  survol.open_file(plain, 30, 0, "tab")
  flush()
  eq(vim.fn.tabpagenr("$"), 1, "no new tab")
  eq(current_path(), vim.fs.normalize(plain))
  eq(vim.api.nvim_win_get_cursor(0)[1], 30)
end)

test("open_file: clamps lines, rejects unreadable paths and bad modes", function()
  survol.open_file(plain, 5000, 0)
  flush()
  eq(vim.api.nvim_win_get_cursor(0)[1], 100)
  assert(survol.open_file(tmp .. "/missing", 1, 0):match("^error: not readable"))
  assert(survol.open_file(plain, 1, 0, "float"):match("^error: unknown open_mode"))
  eq(survol.open_file(plain, 1, 0, "hide"), "ok", "hide = edit")
  flush()
end)

-- Same expression as crates/survol-tui/src/editor.rs `plugin_expr`.
local function plugin_expr(path, line, col)
  local lua = "(function(a) local ok, s = pcall(require, 'survol') "
    .. "if not ok or type(s.open_file) ~= 'function' then return 'noplugin' end "
    .. "return s.open_file(a[1], a[2], a[3]) end)(_A)"
  local function q(s)
    return "'" .. s:gsub("'", "''") .. "'"
  end
  return ("luaeval(%s, [%s, %d, %d])"):format(q(lua), q(path), line, col)
end

-- Runs `nvim --server <us> --remote-expr expr` the way the TUI does.
local function remote_expr(expr)
  local res
  vim.system(
    { vim.v.progpath, "--server", vim.v.servername, "--remote-expr", expr },
    { text = true },
    function(r)
      res = r
    end
  )
  assert(vim.wait(5000, function()
    return res ~= nil
  end), "remote-expr timed out")
  return res
end

test("RPC: path with spaces and quotes, from terminal mode", function()
  survol.open()
  vim.cmd.startinsert()
  local res = remote_expr(plugin_expr(spaced, 7, 3))
  eq(res.code, 0, res.stderr)
  eq(vim.trim(res.stdout), "ok")
  flush()
  assert(not survol.is_visible(), "float hidden")
  eq(current_path(), vim.fs.normalize(spaced))
  eq(vim.api.nvim_win_get_cursor(0), { 7, 2 })
end)

test("RPC: errors come back as text", function()
  local res = remote_expr(plugin_expr(tmp .. "/nope's", 1, 0))
  eq(res.code, 0)
  assert(vim.trim(res.stdout):match("^error: not readable"), res.stdout)
end)

test("RPC: noplugin when the module is missing", function()
  local saved = package.loaded.survol
  local path = package.path
  package.loaded.survol = nil
  vim.opt.rtp:remove(vim.fn.getcwd() .. "/nvim")
  package.preload.survol = function()
    error("gone")
  end
  local res = remote_expr(plugin_expr(plain, 1, 0))
  package.preload.survol = nil
  package.loaded.survol = saved
  package.path = path
  vim.opt.rtp:prepend(vim.fn.getcwd() .. "/nvim")
  eq(vim.trim(res.stdout), "noplugin")
end)

test("job exit closes the float and forgets the job", function()
  survol.setup({ cmd = "sh", args = { "-c", "exit 0", "sh" } })
  survol.open()
  assert(vim.wait(3000, function()
    return not survol.is_running() and not survol.is_visible()
  end), "float closed after exit")
  eq(survol._state(), {})
end)

test("completion", function()
  eq(survol.complete("--n", "Survol --n"), { "--no-llm" })
  assert(vim.tbl_contains(survol.complete("f", "Survol --lang f"), "fr"))
end)

vim.fn.delete(tmp, "rf")
io.stdout:write(failures == 0 and "all tests passed\n" or (failures .. " failure(s)\n"))
vim.cmd(failures == 0 and "qall!" or "cquit 1")
