local M = {}

local function notify(message, level)
  vim.notify("Kette: " .. message, level or vim.log.levels.INFO)
end

local function find_root()
  local start = vim.api.nvim_buf_get_name(0)
  if start == "" then
    start = vim.fn.getcwd()
  else
    start = vim.fn.fnamemodify(start, ":p:h")
  end
  local directory = start
  while true do
    if vim.fn.filereadable(directory .. "/kette/std/startup.ktt") == 1 then
      return directory
    end
    local parent = vim.fn.fnamemodify(directory, ":h")
    if parent == directory then
      return nil
    end
    directory = parent
  end
end

local function install_highlights()
  local links = {
    keyword = "@keyword",
    variable = "@variable",
    parameter = "@variable.parameter",
    property = "@property",
    ["function"] = "@function",
    method = "@function.method",
    namespace = "@module",
    enumMember = "@constant",
    string = "@string",
    number = "@number",
    comment = "@comment",
    operator = "@operator",
  }
  for name, target in pairs(links) do
    vim.api.nvim_set_hl(0, "@lsp.type." .. name, { link = target, default = true })
  end
end

function M.restart()
  for _, lsp in ipairs(vim.lsp.get_clients({ name = "kette" })) do
    lsp:stop(true)
  end
  vim.defer_fn(function()
    M.start()
  end, 100)
end

function M.start()
  install_highlights()
  local root = find_root()
  if not root then
    notify("cannot find kette/std/startup.ktt", vim.log.levels.ERROR)
    return
  end
  local server = vim.g.kette_server or "kettevm"
  local script = root .. "/kette/tools/kettels.ktt"
  local quoted = vim.fn.shellescape
  local command = string.format(
    "cd %s && exec %s %s -- --stdio",
    quoted(root),
    quoted(server),
    quoted(script)
  )
  local id = vim.lsp.start({
    name = "kette",
    cmd = { vim.o.shell, "-c", command },
    root_dir = root,
    on_attach = function(client, bufnr)
      if vim.fn.has("nvim-0.10") == 0
        and client.server_capabilities.semanticTokensProvider
        and vim.lsp.semantic_tokens
        and vim.lsp.semantic_tokens.start
      then
        vim.lsp.semantic_tokens.start(bufnr, client.id)
      end
    end,
  })
  if not id then
    notify("image did not start", vim.log.levels.ERROR)
    return nil
  end
  return vim.lsp.get_client_by_id(id)
end

local function client()
  local clients = vim.lsp.get_clients({ name = "kette", bufnr = 0 })
  if #clients == 0 then
    return M.start()
  end
  return clients[1]
end

local function show(title, text)
  local buffer = vim.api.nvim_create_buf(false, true)
  vim.bo[buffer].buftype = "nofile"
  vim.bo[buffer].bufhidden = "wipe"
  vim.bo[buffer].swapfile = false
  vim.bo[buffer].filetype = "kette-transcript"
  local lines = vim.split(text or "", "\n", { plain = true })
  vim.api.nvim_buf_set_lines(buffer, 0, -1, false, lines)
  vim.cmd("botright split")
  vim.api.nvim_win_set_buf(0, buffer)
  vim.api.nvim_buf_set_name(buffer, title .. "-" .. buffer)
  vim.keymap.set("n", "q", "<cmd>close<cr>", { buffer = buffer, silent = true })
  vim.keymap.set("n", "<esc>", "<cmd>close<cr>", { buffer = buffer, silent = true })
end

local function request(method, params, on_result)
  local lsp = client()
  if not lsp then
    notify("no image connection", vim.log.levels.ERROR)
    return
  end
  lsp:request(method, params, function(err, result)
    if err then
      notify(err.message or vim.inspect(err), vim.log.levels.ERROR)
      return
    end
    on_result(result)
  end, 0)
end

local function show_eval(label, result)
  if not result then
    return
  end
  if result.transcript and result.transcript ~= "" then
    vim.api.nvim_echo({ { result.transcript, "Normal" } }, false, {})
  end
  if result.ok then
    notify(label .. ": " .. tostring(result.value))
  else
    notify(label .. " failed: " .. tostring(result.error), vim.log.levels.ERROR)
  end
end

local function buffer_source(range)
  local first = 1
  local last = vim.api.nvim_buf_line_count(0)
  if range and #range == 2 then
    first = range[1]
    last = range[2]
  else
    first = vim.api.nvim_win_get_cursor(0)[1]
    last = first
  end
  local lines = vim.api.nvim_buf_get_lines(0, first - 1, last, false)
  return table.concat(lines, "\n")
end

function M.eval(range)
  local source = buffer_source(range)
  request("kette/eval", { source = source, uri = vim.uri_from_bufnr(0) }, function(result)
    show_eval("eval", result)
  end)
end

function M.eval_buffer()
  local source = table.concat(vim.api.nvim_buf_get_lines(0, 0, -1, false), "\n")
  request("kette/eval", { source = source, uri = vim.uri_from_bufnr(0) }, function(result)
    show_eval("eval buffer", result)
  end)
end

function M.load_file()
  local path = vim.api.nvim_buf_get_name(0)
  if path == "" then
    notify("buffer has no file", vim.log.levels.ERROR)
    return
  end
  request("kette/loadFile", { path = path }, function(result)
    show_eval("load", result)
  end)
end

function M.transcript()
  request("kette/transcript", {}, function(result)
    show("kette-transcript", result.text or "")
  end)
end

function M.info()
  request("kette/imageInfo", {}, function(result)
    show("kette-image-info", vim.inspect(result))
  end)
end

return M
