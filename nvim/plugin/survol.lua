-- Commands work without setup(); setup() only changes options and keymaps.
if vim.g.loaded_survol then
  return
end
vim.g.loaded_survol = 1
require("survol").define_commands()
