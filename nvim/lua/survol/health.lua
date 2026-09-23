-- :checkhealth survol
local M = {}

function M.check()
  local h = vim.health
  local survol = require("survol")
  h.start("survol")
  if vim.fn.has("nvim-0.10") == 1 then
    h.ok("Neovim " .. tostring(vim.version()))
  else
    h.error("Neovim >= 0.10 required")
  end

  local cmd = survol.config.cmd
  local path = vim.fn.exepath(cmd)
  if path == "" then
    h.error("`" .. cmd .. "` not found in $PATH", { "cargo install --path crates/survol-tui" })
  else
    local res = vim.system({ path, "--version" }, { text = true }):wait(5000)
    if res.code == 0 then
      h.ok(vim.trim(res.stdout) .. " (" .. path .. ")")
    else
      h.warn("`" .. path .. " --version` failed: " .. vim.trim(res.stderr or ""))
    end
  end

  local server = vim.v.servername
  if server ~= nil and server ~= "" then
    h.ok("RPC server: " .. server .. " (survol opens files here through $NVIM)")
  else
    h.error("no RPC server: survol cannot open files in this Neovim")
  end

  if vim.fn.executable("survol-cli") == 1 then
    h.info("`survol-cli doctor` checks git, GitLab and the LLM CLI")
  end
  h.info("open_mode: " .. tostring(survol.config.open_mode))
end

return M
