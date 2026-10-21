-- Rex integration for ai-usage: a "Toggle AI Usage" action that floats an
-- opaque bar across the top of the current tab, showing today's usage for
-- each provider, and removes it again.
--
-- In ~/.config/rex/init.lua:
--
--   package.path = "/path/to/ai-usage/rex/?.lua;" .. package.path
--   require("ai_usage").setup{ bin = "/path/to/ai-usage" }
--
-- Then run `rex config reload`.
--
-- Why the top: Rex.app draws a floating pane only as far as x = w and
-- y = h, as if they were its right and bottom edges, and the server needs
-- x + w <= 1 and y + h <= 1. A pane anchored at the top-left corner is the
-- one kind those agree on, so the bar can't sit along the bottom yet.
-- https://discord.com/channels/1546797889009881158/1559283062016311309

local M = {}

local TERMINAL = "com.superlogical.terminal"

-- Marks the bar so the action can find it again to close it.
local LABEL = "AI usage"

local function active_window(view)
  for _, window in ipairs(view.windows or {}) do
    if window.window_id == view.active_window_id then
      return window
    end
  end
end

local function find_bar(window)
  for _, layer in ipairs(window.layers or {}) do
    for _, block in ipairs(layer.blocks or {}) do
      if block.label == LABEL then
        return block.block_id
      end
    end
  end
end

-- Every block in a layout, with its rect.
local function blocks_in(node, out)
  if node.block_id ~= nil then
    out[#out + 1] = node
  elseif node.split ~= nil then
    blocks_in(node.split.before, out)
    blocks_in(node.split.after, out)
  end
  return out
end

-- The window's height in rows, from any of its panes.
local function window_rows(window)
  for _, layer in ipairs(window.layers or {}) do
    if layer.kind == "tiled" and layer.layout ~= nil then
      local first = blocks_in(layer.layout, {})[1]
      if first ~= nil then
        local size = rex.block.call(TERMINAL, "size", { block_id = first.block_id })
        if size ~= nil and size.rows ~= nil then
          return size.rows / first.rect[4]
        end
      end
    end
  end
end

-- Options, all optional:
--   bin         path to the ai-usage binary (default: ai-usage on the server's PATH)
--   interval    seconds between rereading usage (default 30)
--   rows        the bar's height when it opens (default 3). It's a share of
--               the window, so it grows and shrinks with it.
--   background  the bar's colour: "black" (the theme's palette black, the
--               default) or "#rrggbb"
--   key         a key to bind the action to, such as "cmd+shift+u"
--   sources     other providers' logged API responses, as name = directory,
--               such as { longwire = "/path/to/longwire-logs" }
function M.setup(opts)
  opts = opts or {}
  local bin = opts.bin or "ai-usage"
  local interval = opts.interval or 30
  local rows = opts.rows or 3
  local background = opts.background or "black"
  local command = { bin, "bar", "--interval", tostring(interval), "--background", background }
  -- Sorted, so the bar's command is the same from one reload to the next.
  local names = {}
  for name in pairs(opts.sources or {}) do
    names[#names + 1] = name
  end
  table.sort(names)
  for _, name in ipairs(names) do
    command[#command + 1] = "--source"
    command[#command + 1] = name .. "=" .. opts.sources[name]
  end

  rex.action{
    name = "ai_usage",
    title = "Toggle AI Usage",
    description = "Show today's usage and plan limits for Claude Code, Codex, OpenCode, Cursor, Grok, Gemini CLI and Qwen Code in a bar across the top.",
    keywords = "tokens limits quota claude codex opencode cursor grok gemini qwen bar",
    run = function(ctx, args)
      local view, err = rex.session.view{}
      if err ~= nil then error(err, 0) end
      local window = active_window(view)
      if window == nil then error("ai_usage: no active window", 0) end

      local open = find_bar(window)
      if open ~= nil then
        local _, close_err = rex.call("block.close", { session_id = ctx.session_id, block_id = open })
        if close_err ~= nil then error(close_err, 0) end
        return
      end

      local total = window_rows(window) or 50
      local height = math.min(math.max(rows / total, 0.02), 0.5)
      local _, new_err = rex.session.new_layer{
        bounds = { 0, 0, 1, height },
        -- Focused, so Esc dismisses it straight away.
        focus = true,
        layout = rex.layout.block{
          flavor = TERMINAL .. ".shell",
          label = LABEL,
          options = {
            command = command,
            shell = "none",
          },
        },
      }
      if new_err ~= nil then error(new_err, 0) end
    end,
  }

  if opts.key ~= nil then
    rex.bind(opts.key, "ai_usage")
  end
end

return M
