-- survol.nvim: open survol in a floating terminal.
-- Files opened from survol land in this Neovim (survol reads $NVIM).
local M = {}

M.config = {
  cmd = "survol",
  width = 0.95,
  height = 0.92,
}

function M.open(target)
  local cols, lines = vim.o.columns, vim.o.lines
  local width = math.floor(cols * M.config.width)
  local height = math.floor(lines * M.config.height)
  local buf = vim.api.nvim_create_buf(false, true)
  local win = vim.api.nvim_open_win(buf, true, {
    relative = "editor",
    width = width,
    height = height,
    col = math.floor((cols - width) / 2),
    row = math.floor((lines - height) / 2),
    style = "minimal",
    border = "rounded",
  })
  local cmd = { M.config.cmd }
  if target and target ~= "" then
    table.insert(cmd, target)
  end
  vim.fn.jobstart(cmd, {
    term = true,
    cwd = vim.fn.getcwd(),
    on_exit = function()
      if vim.api.nvim_win_is_valid(win) then
        vim.api.nvim_win_close(win, true)
      end
    end,
  })
  vim.cmd.startinsert()
end

function M.setup(opts)
  M.config = vim.tbl_extend("force", M.config, opts or {})
  vim.api.nvim_create_user_command("Survol", function(args)
    M.open(args.args)
  end, { nargs = "?", desc = "Review a merge request with survol" })
end

return M
