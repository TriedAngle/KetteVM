local kette = require("kette")

vim.api.nvim_create_user_command("KetteStart", function()
  kette.start()
end, {})

vim.api.nvim_create_user_command("KetteRestart", function()
  kette.restart()
end, {})

vim.api.nvim_create_user_command("KetteEval", function(options)
  local range = options.range > 0 and { options.line1, options.line2 } or nil
  kette.eval(range)
end, { range = true })

vim.api.nvim_create_user_command("KetteEvalBuffer", function()
  kette.eval_buffer()
end, {})

vim.api.nvim_create_user_command("KetteLoad", function()
  kette.load_file()
end, {})

vim.api.nvim_create_user_command("KetteTranscript", function()
  kette.transcript()
end, {})

vim.api.nvim_create_user_command("KetteInfo", function()
  kette.info()
end, {})

vim.api.nvim_create_autocmd("FileType", {
  pattern = "kette",
  callback = function()
    kette.start()
  end,
})
