-- pult: WezTerm config.
--
-- Claude Code hooks (`pult hook`) set two user vars on the pane through OSC 1337 SetUserVar:
--   pult_status = working | waiting | done | error | idle | ""      pult_since = unix time
-- The whole tab is coloured from them. Every tab is a pultd session (`pult new`); the `pult_session` user var
-- says which one. Install: ~/.config/wezterm/wezterm.lua is a symlink to this file (scripts/install.sh does it).
--
-- Personal settings go into ~/.config/pult/wezterm.lua, which returns a table. All keys are optional:
--   return {
--     font = 'Iosevka',                  -- terminal font (default: WezTerm's built-in JetBrains Mono)
--     font_size = 12,
--     host_alias = { devbox = 'dev' },   -- hostname -> name shown in tab titles and lists
--     backgrounds = false,               -- false: plain background instead of the generated images
--     bg_font = '/path/to/Mono-Bold.ttf' -- font for the word drawn into the generated backgrounds
--   }

local wezterm = require 'wezterm'
local act = wezterm.action
local config = wezterm.config_builder()

local opts = (function()
  local ok, t = pcall(dofile, wezterm.home_dir .. '/.config/pult/wezterm.lua')
  return (ok and type(t) == 'table') and t or {}
end)()
local font_size = opts.font_size or 11.5
local host_alias = opts.host_alias or {}

-- Colours follow the system light / dark setting and are re-read when it changes. Test: PULT_APPEARANCE=Light|Dark.
-- The config is evaluated in several places (GUI, mux) and wezterm.gui is not available in all of them, so on
-- Linux the GNOME setting is read directly, and WezTerm's own appearance is the fallback (macOS).
local function detect_appearance()
  local forced = os.getenv('PULT_APPEARANCE')
  if forced then return forced end
  local okp, success, out = pcall(wezterm.run_child_process, { 'gsettings', 'get', 'org.gnome.desktop.interface', 'color-scheme' })
  if okp and success and type(out) == 'string' and out:find("'") then
    return out:find('dark') and 'Dark' or 'Light'
  end
  if wezterm.gui then
    local ok2, a = pcall(wezterm.gui.get_appearance)
    if ok2 and a then return a end
  end
  return 'Light'
end
local dark = detect_appearance():find('Dark') ~= nil
local c = dark and {
  crust = '#221f22', mantle = '#19181a', bg = '#2d2a2e', surface = '#403e41', surface2 = '#5b595c',
  overlay = '#727072', sub = '#939293', fg = '#fcfcfa',
  red = '#ff6188', orange = '#fc9867', yellow = '#ffd866', green = '#a9dc76', blue = '#78dce8', magenta = '#ab9df2',
} or {
  crust = '#ece6e4', mantle = '#f2ecea', bg = '#faf4f2', surface = '#e6e0de', surface2 = '#d3cdcc',
  overlay = '#918c8e', sub = '#706b6e', fg = '#29242a',
  red = '#e14775', orange = '#e16032', yellow = '#cc7a0a', green = '#269d69', blue = '#1c8ca8', magenta = '#5a3fa8',
}
-- tab background: this share of the status colour is mixed into the base colour
local tab_mix = { active = 0.34, inactive = 0.16 }

local status_color = {
  -- in the light theme the "blue" is teal, too close to the green of "done"
  working = dark and c.blue or '#3170c9',
  waiting = c.yellow, waiting_pulse = c.orange,
  done = c.green,
  error = c.red,
  idle = c.overlay,
}
local status_icon = { working = '⠋', waiting = '●', done = '✓', error = '✕', idle = '○' }

-- Background: a generated, blurred image (colour blobs, circles, the word "pult") seen through a veil in the
-- theme colour. Every tab gets its own image from its session id. `bg.py` makes the images once into
-- ~/.cache/pult/bg; until they exist the background is plain. Needs python3 with Pillow; opts.backgrounds = false
-- turns it off.
local bg_dir = wezterm.home_dir .. '/.cache/pult/bg'
local bg_count = 12
local bg_prefix = (dark and 'dark-' or 'light-') .. c.mantle:sub(2) .. '-v3'  -- bump the suffix after changing bg.py
local bg_veil = dark and 0.68 or 0.6
local function bg_file(i) return string.format('%s/%s-%d.png', bg_dir, bg_prefix, i) end
local function bg_layers(i)
  return {
    { source = { File = bg_file(i) }, width = 'Cover', height = 'Cover',
      horizontal_align = 'Center', vertical_align = 'Middle' },
    { source = { Color = c.bg }, width = '100%', height = '100%', opacity = bg_veil },
  }
end
local function exists(p) local f = io.open(p, 'rb'); if f then f:close(); return true end; return false end
local use_bg = opts.backgrounds ~= false
local bg_ready = use_bg and exists(bg_file(bg_count))
if bg_ready then
  config.background = bg_layers(1)
elseif use_bg and wezterm.gui then
  -- bg.py sits next to this file (the config is a symlink into the repo); the mkdir lock keeps several
  -- evaluations of the config from starting it more than once
  local ok, _, real = pcall(wezterm.run_child_process, { 'readlink', '-f', wezterm.config_file })
  local script = ok and real and real:gsub('%s+$', ''):gsub('[^/]+$', 'bg.py')
  if script and exists(script) then
    wezterm.background_child_process { 'sh', '-c',
      'mkdir -p "$1" && mkdir "$1/.lock" 2>/dev/null || exit 0; PULT_BG_FONT="$2" python3 "$3" "$1" "$4" ' .. bg_count
        .. ' "$5" "$6" "$7" "$8" "$9" "${10}" "${11}" "${12}"; rmdir "$1/.lock"',
      'bg', bg_dir, opts.bg_font or '', script, bg_prefix,
      c.mantle, c.magenta, c.blue, c.blue, c.yellow, c.orange, c.green, c.red }
  end
end
-- translucent colour (only with a background image): the image shows through the tab bar and tabs
local function glass(color, alpha)
  if not bg_ready then return color end
  local r, g, b = tonumber(color:sub(2, 3), 16), tonumber(color:sub(4, 5), 16), tonumber(color:sub(6, 7), 16)
  return string.format('rgba(%d, %d, %d, %.2f)', r, g, b, alpha)
end
local glass_alpha = { bar = 0.3, inactive = 0.5, active = 0.65 }

config.colors = {
  foreground = c.fg,
  background = c.bg,
  cursor_bg = c.fg, cursor_fg = c.bg, cursor_border = c.fg,
  selection_bg = c.surface2, selection_fg = c.fg,
  scrollbar_thumb = c.surface2,
  split = c.surface2,
  -- in the light theme "black" is the dark text colour, in the dark theme the other way round
  ansi    = dark and { c.surface, c.red, c.green, c.yellow, c.blue, c.magenta, c.blue, c.fg }
                 or { c.fg, c.red, c.green, c.yellow, c.blue, c.magenta, c.blue, c.sub },
  brights = dark and { c.overlay, c.red, c.green, c.orange, c.blue, c.magenta, c.blue, c.fg }
                 or { c.overlay, c.red, c.green, c.orange, c.blue, c.magenta, c.blue, c.sub },
  tab_bar = {
    background = glass(c.crust, glass_alpha.bar),
    new_tab = { bg_color = glass(c.crust, glass_alpha.bar), fg_color = c.overlay },
    new_tab_hover = { bg_color = glass(c.surface, glass_alpha.active), fg_color = c.fg },
    inactive_tab_edge = glass(c.crust, glass_alpha.bar),
  },
}

-- Font and window ---------------------------------------------------------------------------------
if opts.font then
  config.font = wezterm.font_with_fallback { opts.font, 'Symbols Nerd Font Mono' }
end
config.font_size = font_size
config.line_height = 1.1

config.use_fancy_tab_bar = true
config.tab_max_width = 44
local frame_base = {
  -- an explicit fallback for the spinner's braille glyphs: without it WezTerm walks the system fallback fonts on
  -- every tab bar repaint (~10 ms per frame, ~10% CPU while the spinner runs)
  font = wezterm.font_with_fallback {
    opts.font or 'JetBrains Mono',
    wezterm.target_triple:find('darwin') and 'Apple Braille' or 'DejaVu Sans',
  },
  -- the same size as the terminal font: the fancy tab bar measures tabs and the right-hand status in terminal
  -- cells, and a larger font truncates the labels
  font_size = font_size,
  active_titlebar_bg = glass(c.crust, glass_alpha.bar),
  inactive_titlebar_bg = glass(c.crust, glass_alpha.bar),
}
config.window_frame = frame_base
-- colours of the pickers (a floating panel in pult-term) and of the command palette
config.command_palette_bg_color = c.mantle
config.command_palette_fg_color = c.fg

-- background image number for a tab, from its session id (the same session always gets the same image)
local function bg_index(pane)
  local key = pane and (pane:get_user_vars().pult_session or tostring(pane:pane_id())) or '1'
  local h = 0
  for i = 1, #key do h = (h * 31 + key:byte(i)) % 1000003 end
  return h % bg_count + 1
end
config.show_new_tab_button_in_tab_bar = true
config.show_close_tab_button_in_tabs = false  -- the × truncated the labels in the fancy tab bar (close: Ctrl+Shift+W)
config.window_padding = { left = 10, right = 10, top = 8, bottom = 6 }
-- Stock WezTerm refreshes the spinner and the pulse from Lua at this interval; pult-term animates natively, so
-- once a second is enough there (status, notifications, clock).
local is_fork = (wezterm.executable_dir or ''):find('pult%-term') ~= nil
config.status_update_interval = is_fork and 1000 or 250
config.audible_bell = 'Disabled'

-- The stable WezTerm 20240203 crashes on GNOME Wayland with fractional scaling ("Buffer size must be an integer
-- multiple of the buffer_scale"), so older versions run on XWayland (blurry). Nightlies and forks use Wayland.
config.enable_wayland = wezterm.version > '20250101'
-- GNOME draws no title bar on Wayland: the window buttons go to the right end of the tab bar, and the window
-- can be dragged by its empty part (double click: maximise).
config.window_decorations = 'INTEGRATED_BUTTONS|RESIZE'
config.integrated_title_button_style = 'Gnome'
config.integrated_title_buttons = { 'Hide', 'Maximize', 'Close' }
config.integrated_title_button_color = c.sub

-- pultd: every new tab is a persistent session; closing the tab only detaches --------------------------------
local pult_bin = wezterm.home_dir .. '/.cargo/bin/pult'
local has_pult = exists(pult_bin)
if has_pult then config.default_prog = { pult_bin, 'new' } end
config.exit_behavior = 'Close'
-- closing the pult client loses nothing (the session keeps running), so there is nothing to confirm
config.skip_close_confirmation_for_processes_named = { 'bash', 'sh', 'zsh', 'fish', 'tmux', 'nu', 'pult' }

-- Helpers ---------------------------------------------------------------------------------------------
local function hex(s) return tonumber(s:sub(2, 3), 16), tonumber(s:sub(4, 5), 16), tonumber(s:sub(6, 7), 16) end
-- mix of a and b: t is the share of a (0..1)
local function mix(a, b, t)
  local r1, g1, b1 = hex(a)
  local r2, g2, b2 = hex(b)
  local f = function(x, y) return math.floor(x * t + y * (1 - t) + 0.5) end
  return string.format('#%02x%02x%02x', f(r1, r2), f(g1, g2), f(b1, b2))
end

local function duration(s)
  if s < 60 then return s .. 's' end
  if s < 3600 then return math.floor(s / 60) .. 'm' end
  if s < 86400 then return math.floor(s / 3600) .. 'h' end
  return math.floor(s / 86400) .. 'd'
end
local function age(since) return duration(os.time() - (tonumber(since) or os.time())) end

local function basename(cwd)
  if not cwd then return nil end
  local p = type(cwd) == 'userdata' and cwd.file_path or tostring(cwd)
  p = p:gsub('/$', '')
  return p:match('([^/]+)$') or p
end

-- shorten the home directory to ~; `home` is the host's own (when known), else the usual /home/<user> and /Users/<user>
local function tilde(path, home)
  if home and home ~= '' and (path == home or path:sub(1, #home + 1) == home .. '/') then
    return '~' .. path:sub(#home + 1)
  end
  return (path:gsub('^/home/[^/]+', '~'):gsub('^/Users/[^/]+', '~'))
end

local function pane_status(pane)
  local s = (pane.user_vars or {}).pult_status
  if s == nil or s == '' then return nil end
  return s
end

local function pane_session(pane)
  local v = pane:get_user_vars().pult_session
  if v and v ~= '' then return v end
end

local function pulse_on() return os.time() % 2 == 0 end

-- Does the pult-term fork animate the tab bar itself (window.pult_tab_anim)? Checked lazily, from the tab title
-- formatter: every config reload is a new Lua state, which has to look again.
local tab_anim_ok
local function tab_anim()
  if tab_anim_ok == nil and wezterm.gui then
    local w = wezterm.gui.gui_windows()[1]
    if w then tab_anim_ok = pcall(function() return w.pult_tab_anim end) end
  end
  return tab_anim_ok == true
end

-- precise time (fractional seconds) for the animations
local function now_f()
  local ok, v = pcall(function() return tonumber(wezterm.time.now():format('%s%.3f')) end)
  return (ok and v) or os.time()
end
local spinner = { '⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏' }
-- 0 → 1 → 0 over 1.6 s (the pulse of a waiting tab)
local function wave(t)
  local p = (t % 1.6) / 1.6
  return p < 0.5 and p * 2 or (1 - p) * 2
end

-- Tab colouring by status ---------------------------------------------------------------------------
-- Inactive tab: the status colour faintly mixed into the tab bar background, dimmer label.
-- Active tab: stronger colour, bold, full-strength label and stripe.
wezterm.on('format-tab-title', function(tab, tabs, panes, cfg, hover, max_width)
  local pane = tab.active_pane
  local st = pane_status(pane)
  local dir = wezterm.truncate_right(basename(pane.current_working_dir) or pane.title, 18)
  local uv = pane.user_vars or {}
  local ph = uv.pult_host
  local is_remote = ph and ph ~= '' and ph ~= wezterm.hostname()
  local t = now_f()

  -- The fork animates by itself (even frames, no Lua): the spinner is a braille glyph it replaces, and a pulsing
  -- tab's title is `A \u{e000} B` (the same label in the colours of the two ends of the pulse). Stock WezTerm
  -- computes it here from the time, at the status_update_interval rate.
  local native = tab_anim()
  -- A "done" tab you have already looked at turns neutral (only the ✓ stays green), otherwise every tab would be
  -- green by evening. Done / error titles end in a `\u{e001}` marker that makes the fork flash the tab once on
  -- the change (natively: WezTerm does not recompute the title over time).
  local seen = wezterm.GLOBAL.pult_seen or {}
  local calm = st == 'idle' or (st == 'done' and not tab.is_active and seen[tostring(pane.pane_id)] == uv.pult_since)
  -- w: position of the pulse (0: base colour, 1: peak)
  local function colors(w)
    local col
    if st then
      col = status_color[st] or c.overlay
      if st == 'waiting' then col = mix(status_color.waiting_pulse, status_color.waiting, w) end
    end
    local bg, fg, dimfg
    if tab.is_active then
      -- without a status the dark background is barely different from the tab bar: one step lighter
      bg = (col and not calm) and mix(col, c.bg, tab_mix.active) or (dark and c.surface or c.bg)
      fg, dimfg = c.fg, c.sub
    else
      bg = (col and not calm) and mix(col, c.crust, tab_mix.inactive) or c.crust
      fg, dimfg = c.sub, c.overlay
    end
    if hover and not tab.is_active then bg = mix(c.fg, bg, 0.06) end
    local stripe = (col and not calm) and col or (tab.is_active and c.fg or c.crust)
    if not tab.is_active and col and not calm then stripe = mix(col, bg, 0.8) end
    bg = glass(bg, tab.is_active and glass_alpha.active or glass_alpha.inactive)
    return { col = col, bg = bg, fg = fg, dimfg = dimfg, stripe = stripe }
  end
  local k = colors(native and 0 or wave(t))

  local icon
  if st == 'working' then icon = native and spinner[1] or spinner[math.floor(t * 8) % #spinner + 1]
  elseif st then icon = status_icon[st] or '?' end
  local a = (st and st ~= 'idle') and age(uv.pult_since) or nil
  if a and a:match('s$') then a = nil end

  -- host name: always for remote tabs, for local ones only if it fits
  local function build1(with_host, k)
    local name = (with_host and ph and ph ~= '') and ((host_alias[ph] or ph) .. ':' .. dir) or dir
    local items = {
      { Background = { Color = k.bg } },
      { Foreground = { Color = k.stripe } }, { Text = '▎' },
      { Foreground = { Color = k.dimfg } }, { Text = ' ' .. (tab.tab_index + 1) .. ' ' },
    }
    local len = 3 + #tostring(tab.tab_index + 1)
    if icon then
      table.insert(items, { Foreground = { Color = k.col or k.dimfg } })
      table.insert(items, { Text = icon .. ' ' })
      len = len + 2
    end
    table.insert(items, { Foreground = { Color = k.fg } })
    table.insert(items, { Attribute = { Intensity = tab.is_active and 'Bold' or 'Normal' } })
    table.insert(items, { Text = name })
    table.insert(items, 'ResetAttributes')
    len = len + utf8.len(name)
    if a then
      table.insert(items, { Background = { Color = k.bg } })
      table.insert(items, { Foreground = { Color = k.dimfg } })
      table.insert(items, { Text = ' ' .. a })
      len = len + 1 + utf8.len(a)
    end
    table.insert(items, { Background = { Color = k.bg } })
    table.insert(items, { Text = ' ' })
    return items, len + 1
  end
  local function build(with_host)
    local items, len = build1(with_host, k)
    if native and st == 'waiting' then
      local b = build1(with_host, colors(1))
      table.insert(items, { Text = '\u{e000}' })
      for _, it in ipairs(b) do table.insert(items, it) end
    end
    if native and (st == 'done' or st == 'error') and not calm then table.insert(items, { Text = '\u{e001}' }) end
    return items, len
  end
  if is_remote then return (build(true)) end
  -- room per tab: the window width (minus ~40 columns for the right-hand status) shared by the tabs
  local budget = math.min(max_width, math.floor(((pane.width or 200) - 40) / math.max(1, #tabs)))
  local items, len = build(true)
  if len <= budget then return items end
  return (build(false))
end)

-- Right-hand status and ambient frame while something waits ---------------------------------------------
local save_order  -- defined below (tab order), called from update-status
local window_born = {}  -- window id → time of the first status update (the key hint shows in new windows)

-- Claude Code plan usage: ~/.claude/statusline.sh writes the rate_limits of the statusline JSON here
-- (see the README), and this config reads it every 10 s.
local usage_file = wezterm.home_dir .. '/.local/state/pult/usage.json'
local usage_cache, usage_read_at = nil, 0
local function read_usage()
  if os.time() - usage_read_at < 10 then return usage_cache end
  usage_read_at = os.time()
  local f = io.open(usage_file, 'r')
  if not f then usage_cache = nil return nil end
  local ok, data = pcall(wezterm.json_parse, f:read('*a'))
  f:close()
  usage_cache = ok and data or nil
  return usage_cache
end

local function pct_color(p)
  if p >= 80 then return c.red end
  if p >= 50 then return c.yellow end
  return c.green
end

local function until_str(ts)
  local s = (tonumber(ts) or 0) - os.time()
  if s <= 0 then return '' end
  return duration(s)
end

local function usage_items(items)
  local u = read_usage()
  if not u or not u.rate_limits then return end
  for _, k in ipairs { { 'five_hour', '5h' }, { 'seven_day', '7d' } } do
    local r = u.rate_limits[k[1]]
    if r and r.used_percentage then
      local p = math.floor(r.used_percentage + 0.5)
      table.insert(items, { Foreground = { Color = c.overlay } })
      table.insert(items, { Text = k[2] .. ' ' })
      table.insert(items, { Foreground = { Color = pct_color(p) } })
      table.insert(items, { Attribute = { Intensity = 'Bold' } })
      table.insert(items, { Text = p .. '%' })
      table.insert(items, 'ResetAttributes')
      -- time until the reset only once it matters (>= 50%)
      local left = p >= 50 and until_str(r.resets_at) or ''
      table.insert(items, { Foreground = { Color = c.overlay } })
      table.insert(items, { Text = (left ~= '' and (' ' .. left) or '') .. '  ' })
    end
  end
end

-- Notifications (pult-term only: window:pult_notify) ------------------------------------------------------
-- When a tab needs permission / is done / failed and you are not looking at it: a desktop notification with
-- buttons. Only on a status change (old "done" tabs stay quiet at startup); it goes away when you look at the tab
-- or the status changes.
local notify_ok          -- does window:pult_notify exist? (nil: not checked yet)
local notif_key = {}     -- pane id → last seen state key
local notif_shown = {}   -- pane id → key of the notification on screen
local fetch_all, find_pane_session  -- defined below (approval checks the daemon)
local notif_ask = {}     -- pane id → the approvable request (since, ask), verified on click
local notif_text = {
  waiting = 'needs permission',
  done = 'done, your turn',
  error = 'failed',
}

local function pane_label(p)
  local uv = p:get_user_vars()
  local dir = basename(p:get_current_working_dir()) or p:get_title()
  local h = uv.pult_host
  if h and h ~= '' then return (host_alias[h] or h) .. ':' .. dir end
  return dir
end

local function update_notifications(window)
  if notify_ok == nil then notify_ok = pcall(function() return window.pult_notify end) end
  if not notify_ok then return end
  local focused = window:is_focused()
  local active = window:active_pane()
  local active_id = active and active:pane_id()
  for _, t in ipairs(window:mux_window():tabs()) do
    for _, p in ipairs(t:panes()) do
      local id = p:pane_id()
      local uv = p:get_user_vars()
      local st = uv.pult_status or ''
      local ask = uv.pult_ask or ''
      -- the request text only matters while waiting (the idle_prompt after "done" must not notify again)
      local key = st .. '|' .. (uv.pult_since or '') .. '|' .. (st == 'waiting' and ask or '')
      local seen = focused and id == active_id
      local tag = 'pane-' .. id
      if notif_shown[id] and (seen or notif_shown[id] ~= key) then
        window:pult_notify_close(tag)
        notif_shown[id] = nil
      end
      if notif_key[id] ~= nil and notif_key[id] ~= key and notif_text[st] and not seen then
        local msg = notif_text[st]
        if st == 'waiting' and ask ~= '' then msg = ask end
        local actions = { { 'default', '' }, { 'open', 'Open' } }
        -- approving is offered only for a one-line, fully visible permission request (not for questions,
        -- forms or truncated commands)
        if st == 'waiting' and uv.pult_ask_kind == 'permission' then
          table.insert(actions, { 'bg-allow', 'Allow' })
          notif_ask[id] = { since = uv.pult_since or '', ask = ask }
        end
        window:pult_notify {
          tag = tag,
          title = pane_label(p) .. ' · ' .. notif_text[st],
          message = msg,
          actions = actions,
        }
        notif_shown[id] = key
      end
      notif_key[id] = key
    end
  end
end

wezterm.on('pult-notification-action', function(window, tag, action)
  local id = tonumber(tag:match('^pane%-(%d+)$'))
  if not id then return end
  for _, mw in ipairs(wezterm.mux.all_windows()) do
    for _, t in ipairs(mw:tabs()) do
      for _, p in ipairs(t:panes()) do
        if p:pane_id() == id then
          notif_shown[id] = nil
          if action == 'bg-allow' then
            -- In Claude's permission prompt "1. Yes" is preselected, so Enter approves. Any output of a tab can set
            -- user vars, so Enter is only sent if the daemon (from the hook's own state file) agrees that exactly the
            -- request shown in the notification is pending. Otherwise the tab is just opened.
            local want = notif_ask[id]
            notif_ask[id] = nil
            local _, s = find_pane_session(fetch_all(), p)
            if want and s and s.status == 'waiting' and s.ask_kind == 'permission' and s.ask == want.ask
              and tonumber(s.since) == tonumber(want.since) then
              p:send_text('\r')
              return
            end
            wezterm.log_warn('pult: approval does not match the pending request, only opening the tab')
          end
          t:activate()
          p:activate()
          return
        end
      end
    end
  end
end)

wezterm.on('update-status', function(window, pane)
  save_order()
  update_notifications(window)
  local waiting = 0
  for _, t in ipairs(window:mux_window():tabs()) do
    for _, p in ipairs(t:panes()) do
      if (p:get_user_vars().pult_status or '') == 'waiting' then waiting = waiting + 1 end
    end
  end

  -- a shorter status in narrow windows, so the tabs fit
  local cols = pane:get_dimensions().cols
  local narrow = cols < 120

  local items = {}
  local wid = window:window_id()
  window_born[wid] = window_born[wid] or os.time()
  -- only with few tabs, otherwise WezTerm would cut it off (a new window has one tab anyway)
  if os.time() - window_born[wid] < 20 and #window:mux_window():tabs() <= 3 then
    table.insert(items, { Foreground = { Color = c.magenta } })
    table.insert(items, { Attribute = { Intensity = 'Bold' } })
    table.insert(items, { Text = 'F1' })
    table.insert(items, 'ResetAttributes')
    table.insert(items, { Foreground = { Color = c.sub } })
    table.insert(items, { Text = ': keys   ' })
  end
  -- In the fork the "waiting" counter pulses natively: `A \u{e000} B` (the whole status twice, in the two end colours).
  local native = tab_anim()
  local function status_text(wcol)
    local its = { { Text = '   ' } }  -- room after the "+" button
    for _, it in ipairs(items) do table.insert(its, it) end
    if waiting > 0 then
      table.insert(its, { Foreground = { Color = wcol } })
      table.insert(its, { Attribute = { Intensity = 'Bold' } })
      table.insert(its, { Text = narrow and ('◆' .. waiting .. '  ') or ('◆ ' .. waiting .. ' waiting   ') })
      table.insert(its, 'ResetAttributes')
    end
    if cols >= 100 then usage_items(its) end
    table.insert(its, { Foreground = { Color = c.overlay } })
    table.insert(its, { Text = wezterm.strftime '%H:%M' .. ' ' })
    return wezterm.format(its)
  end
  if native and waiting > 0 then
    window:set_right_status(status_text(status_color.waiting) .. '\u{e000}' .. status_text(status_color.waiting_pulse))
  else
    window:set_right_status(status_text(pulse_on() and status_color.waiting_pulse or status_color.waiting))
  end

  -- remember a "done" state seen on the focused window's active tab (see format-tab-title)
  if window:is_focused() and (pane:get_user_vars().pult_status or '') == 'done' then
    local seen = wezterm.GLOBAL.pult_seen or {}
    local key = tostring(pane:pane_id())
    if seen[key] ~= pane:get_user_vars().pult_since then
      local copy = {}
      for k2, v2 in pairs(seen) do copy[k2] = v2 end
      copy[key] = pane:get_user_vars().pult_since
      wezterm.GLOBAL.pult_seen = copy
    end
  end

  -- Ambient frame: while something waits, the window frame has the waiting colour (constant: recolouring means a
  -- config reload; the pulse lives on the tab). Setting an override reloads the config (new Lua state, local
  -- variables are lost), so we compare with the existing override and only set it when it differs, or the config
  -- would reload forever (~90% CPU).
  local overrides = window:get_config_overrides() or {}
  local changed = false
  -- the tab's background image (only on change: every override reloads the config)
  bg_ready = bg_ready or (use_bg and exists(bg_file(bg_count)))  -- on first start the images are still being made
  if bg_ready then
    local want_bg = bg_file(bg_index(window:active_pane()))
    local cur_bg = overrides.background and overrides.background[1].source.File
    if cur_bg ~= want_bg then
      overrides.background = bg_layers(bg_index(window:active_pane()))
      changed = true
    end
  end
  local cur = overrides.window_frame and overrides.window_frame.border_top_color or 'off'
  local want = waiting > 0 and status_color.waiting or 'off'
  if cur ~= want then
    if want == 'off' then
      overrides.window_frame = nil
    else
      local frame = {}
      for k, v in pairs(frame_base) do frame[k] = v end
      frame.border_left_width, frame.border_right_width = '3px', '3px'
      frame.border_bottom_height, frame.border_top_height = '3px', '3px'
      frame.border_left_color, frame.border_right_color = want, want
      frame.border_bottom_color, frame.border_top_color = want, want
      overrides.window_frame = frame
    end
    changed = true
  end
  if changed then window:set_config_overrides(overrides) end
end)

-- Keys ------------------------------------------------------------------------------------------------
-- Ctrl+Shift+I: jump to the next tab (in this window) where Claude waits for you
local function jump_waiting(window, pane)
  local tabs = window:mux_window():tabs_with_info()
  local cur = 0
  for i, t in ipairs(tabs) do if t.is_active then cur = i end end
  for k = 1, #tabs do
    local i = (cur + k - 1) % #tabs + 1
    for _, p in ipairs(tabs[i].tab:panes()) do
      if (p:get_user_vars().pult_status or '') == 'waiting' then
        tabs[i].tab:activate()
        return
      end
    end
  end
  window:toast_notification('pult', 'Nothing is waiting for you', nil, 2000)
end

-- Sessions on every host ------------------------------------------------------------------------------
-- `pult ls --all --json`: this machine plus the hosts of ~/.config/pult/hosts (SSH, in parallel, ~2 s timeout).
local my_host = wezterm.hostname()

local function pane_host(pane)
  local v = pane:get_user_vars().pult_host
  if v and v ~= '' then return v end
  return my_host
end

fetch_all = function()
  local ok, out = wezterm.run_child_process { pult_bin, 'ls', '--all', '--json' }
  local okj, hosts = pcall(wezterm.json_parse, ok and out or '[]')
  if not okj or type(hosts) ~= 'table' then hosts = {} end
  return hosts
end

-- only this machine's daemon: instant, unlike `fetch_all` (which waits for hosts that do not answer)
local function fetch_local()
  local ok, out = wezterm.run_child_process { pult_bin, 'ls', '--json' }
  local okj, list = pcall(wezterm.json_parse, ok and out or '[]')
  return { { ['local'] = true, sessions = okj and type(list) == 'table' and list or {} } }
end

-- where a session lives: (host name in the pult config, hostname, id) → target of the pult commands (`devbox:3` / `3`)
local function spec_of(h, s) return h['local'] and tostring(s.id) or (h.name .. ':' .. s.id) end

-- Tab order and restore at startup --------------------------------------------------------------------
-- The order of the open tabs (host|session id) is saved to ~/.local/state/pult/tabs.json. At startup every live
-- session that is open nowhere (on any host) gets a tab, in that order.
local order_file = wezterm.home_dir .. '/.local/state/pult/tabs.json'
local order_str = nil
local order_key = nil

local function read_order()
  local f = io.open(order_file, 'r')
  if not f then return {} end
  local ok, v = pcall(wezterm.json_parse, f:read('*a'))
  f:close()
  return (ok and type(v) == 'table') and v or {}
end

save_order = function()
  local cur, seen = {}, {}
  for _, w in ipairs(wezterm.mux.all_windows()) do
    for _, t in ipairs(w:tabs()) do
      local p = t:active_pane()
      local sid = pane_session(p)
      local k = sid and (pane_host(p) .. '|' .. sid)
      if k and not seen[k] then seen[k] = true table.insert(cur, k) end
    end
  end
  if #cur == 0 then return end  -- on close (or without pult) the old order stays
  -- cheap pre-check: nothing to save if the open tabs are the same
  local key = table.concat(cur, ',')
  if key == order_key then return end
  order_key = key
  -- the sessions that are not open now (detached, or tabs just closing) follow in their old order
  local old = order_str and wezterm.json_parse(order_str) or read_order()
  for _, k in ipairs(old) do
    if not seen[k] and #cur < 100 then seen[k] = true table.insert(cur, k) end
  end
  local s = wezterm.json_encode(cur)
  if s == order_str then return end
  order_str = s
  local f = io.open(order_file, 'w')
  if f then f:write(s) f:close() end
end

-- sessions open nowhere, in the saved order (unknown ones: local first, then by creation time)
local function restore_specs()
  local pos = {}
  for i, k in ipairs(read_order()) do pos[k] = i end
  local list = {}
  for _, h in ipairs(fetch_all()) do
    for _, s in ipairs(h.sessions or {}) do
      if (s.clients or 0) == 0 then
        local hostname = (type(s.host) == 'string' and s.host) or my_host
        table.insert(list, { spec = spec_of(h, s), pos = pos[hostname .. '|' .. s.id], loc = h['local'] == true, created = s.created or 0 })
      end
    end
  end
  table.sort(list, function(a, b)
    if (a.pos ~= nil) ~= (b.pos ~= nil) then return a.pos ~= nil end
    if a.pos then return a.pos < b.pos end
    if a.loc ~= b.loc then return a.loc end
    return a.created < b.created
  end)
  local out = {}
  for _, x in ipairs(list) do table.insert(out, x.spec) end
  return out
end

wezterm.on('gui-startup', function(cmd)
  local mux = wezterm.mux
  -- `wezterm start -- command`: run that, restore nothing
  if not has_pult or (cmd and cmd.args) then mux.spawn_window(cmd or {}) return end
  local specs = restore_specs()
  if #specs == 0 then mux.spawn_window(cmd or {}) return end
  local first, _, window = mux.spawn_window { args = { pult_bin, 'attach', specs[1] } }
  for i = 2, #specs do window:spawn_tab { args = { pult_bin, 'attach', specs[i] } } end
  first:activate()
end)

-- the session of a pane in the query result: (host block, session) or nil
find_pane_session = function(hosts, pane)
  local sid, host = pane_session(pane), pane_host(pane)
  for _, h in ipairs(hosts) do
    for _, s in ipairs(h.sessions or {}) do
      local shost = (type(s.host) == 'string' and s.host) or (h['local'] and my_host) or nil
      if tostring(s.id) == sid and shost == host then return h, s end
    end
  end
end

-- the host name in the pult config for a hostname
local function host_name_for(hosts, hostname)
  for _, h in ipairs(hosts) do
    if h['local'] and hostname == my_host then return h.name end
    for _, s in ipairs(h.sessions or {}) do
      if s.host == hostname then return h.name end
    end
  end
  return host_alias[hostname] or hostname
end

local status_order = { waiting = 1, error = 2, done = 3, working = 4, idle = 5 }
local function colored(col, text) return wezterm.format { { Foreground = { Color = col } }, { Text = text } } end

-- one row per session: host, status, directory / name, what runs in it, topic (Claude puts it in the terminal title)
local function session_rows(hosts)
  -- dim parts: lighter grey in the dark theme, so they stay readable in the selected (grey) row
  local dim = dark and c.sub or c.overlay
  local rows = {}
  for _, h in ipairs(hosts) do
    for _, s in ipairs(h.sessions or {}) do table.insert(rows, { h = h, s = s }) end
  end
  local function st_of(x) return (type(x.status) == 'string' and x.status ~= '') and x.status or nil end
  table.sort(rows, function(x, y)
    local ox, oy = status_order[st_of(x.s) or ''] or 9, status_order[st_of(y.s) or ''] or 9
    if ox ~= oy then return ox < oy end
    if x.h.name ~= y.h.name then
      if (x.h['local'] == true) ~= (y.h['local'] == true) then return x.h['local'] == true end
      return x.h.name < y.h.name
    end
    return (x.s.id or 0) < (y.s.id or 0)
  end)
  local hw = 0
  for _, h in ipairs(hosts) do hw = math.max(hw, #h.name) end
  local choices = {}
  for _, r in ipairs(rows) do
    local h, s = r.h, r.s
    local st = st_of(s)
    local name = type(s.name) == 'string' and s.name or basename(s.cwd) or '?'
    local cmd = type(s.cmd) == 'string' and s.cmd or 'shell'
    local title = type(s.title) == 'string' and s.title or ''
    -- a shell's own title (e.g. "user@host: ~/dir") says nothing new, only a program's does
    if cmd == 'shell' or title == cmd then title = '' end
    local items = {
      { Foreground = { Color = h['local'] and (dark and c.fg or c.sub) or c.blue } }, { Text = string.format('%-' .. hw .. 's  ', h.name) },
      { Foreground = { Color = st and status_color[st] or dim } }, { Text = (st and status_icon[st] or '·') .. ' ' },
      { Foreground = { Color = c.fg } }, { Attribute = { Intensity = 'Bold' } }, { Text = name },
      'ResetAttributes',
      { Foreground = { Color = cmd == 'shell' and dim or c.magenta } }, { Text = '  ' .. cmd },
    }
    if title ~= '' then
      table.insert(items, { Foreground = { Color = c.fg } })
      table.insert(items, { Text = '  ' .. wezterm.truncate_right(title, 48) })
    end
    if st == 'idle' then
      table.insert(items, { Foreground = { Color = dim } })
      table.insert(items, { Text = '  idle' })
    elseif st then
      local label = ({ waiting = 'needs permission', working = 'working', done = 'done, your turn', error = 'failed' })[st] or st
      table.insert(items, { Foreground = { Color = status_color[st] } })
      table.insert(items, { Text = '  ' .. label .. ' ' .. age(s.since) })
    end
    table.insert(items, { Foreground = { Color = dim } })
    table.insert(items, { Text = '  ' .. tilde(s.cwd or '', s.home) .. ((s.clients or 0) > 0 and '  · open' or '') .. '  #' .. s.id })
    local hostname = (type(s.host) == 'string' and s.host) or my_host
    table.insert(choices, { id = 'a|' .. spec_of(h, s) .. '|' .. hostname .. '|' .. s.id, label = wezterm.format(items) })
  end
  for _, h in ipairs(hosts) do
    if not h.ok then
      table.insert(choices, { id = 'x', label = colored(c.overlay, string.format('%-' .. hw .. 's  unreachable (%s)', h.name, tostring(h.error))) })
    end
  end
  return choices
end

-- new session: host → directory → shell or Claude
local function new_session_flow(window, pane, host_name, is_local)
  local args = { pult_bin, 'dirs' }
  if not is_local then args = { pult_bin, 'dirs', '--host', host_name } end
  local ok, out = wezterm.run_child_process(args)
  if not ok then
    window:toast_notification('pult', host_name .. ': unreachable', nil, 3000)
    return
  end
  local choices = {}
  local home  -- `pult dirs` lists the host's home first
  for d in out:gmatch('[^\n]+') do
    home = home or d
    table.insert(choices, { id = d, label = tilde(d, home) })
  end
  window:perform_action(act.InputSelector {
    title = 'New session: ' .. host_name .. ', directory',
    fuzzy = true,
    fuzzy_description = host_name .. ' directory: ',
    choices = choices,
    action = wezterm.action_callback(function(w, p, dir)
      if not dir then return end
      w:perform_action(act.InputSelector {
        title = 'New session: ' .. host_name .. ' · ' .. dir,
        choices = {
          { id = 'claude', label = 'claude' },
          { id = 'shell', label = 'shell' },
        },
        action = wezterm.action_callback(function(w2, p2, mode)
          if not mode then return end
          local cmd = { pult_bin, 'new', '--host', host_name, '--cwd', dir }
          if mode == 'claude' then table.insert(cmd, '--run') table.insert(cmd, 'claude') end
          w2:perform_action(act.SpawnCommandInNewTab { args = cmd }, p2)
        end),
      }, p)
    end),
  }, pane)
end

-- F5: pick the host for a new session
local function new_on_host(window, pane)
  local hosts = fetch_all()
  local choices = {}
  for _, h in ipairs(hosts) do
    table.insert(choices, {
      id = (h['local'] and 'L|' or 'R|') .. h.name,
      label = h.ok and colored(h['local'] and c.fg or c.blue, h.name .. (h['local'] and '  (this machine)' or ''))
        or colored(c.overlay, h.name .. '  unreachable'),
    })
  end
  window:perform_action(act.InputSelector {
    title = 'New session: which host?',
    choices = choices,
    action = wezterm.action_callback(function(w, p, id)
      if not id then return end
      new_session_flow(w, p, id:sub(3), id:sub(1, 1) == 'L')
    end),
  }, pane)
end

-- picker for stopping a session (from the bottom of the Ctrl+Shift+O list)
local function kill_picker(window, pane)
  local choices = session_rows(fetch_all())
  window:perform_action(act.InputSelector {
    title = 'Stop a session (whatever runs in it exits)',
    fuzzy_description = 'stop: ',
    choices = choices,
    action = wezterm.action_callback(function(w, p, id)
      local spec = id and id:match('^a|([^|]+)|')
      if spec then wezterm.run_child_process { pult_bin, 'kill', spec } end
    end),
  }, pane)
end

-- Ctrl+Shift+O / F2: sessions on every host, those waiting for you first; one already open in this window is
-- activated instead of opened again
local function session_picker(window, pane)
  local hosts = fetch_all()
  local choices = session_rows(hosts)
  table.insert(choices, { id = 'here', label = colored(c.green, '+  new session here') })
  for _, h in ipairs(hosts) do
    if h.ok then
      table.insert(choices, { id = (h['local'] and 'nL|' or 'nR|') .. h.name, label = colored(c.green, '+  new session: ' .. h.name .. '…') })
    end
  end
  table.insert(choices, { id = 'kill', label = colored(c.red, '✕  stop a session…') })
  window:perform_action(act.InputSelector {
    title = 'pult sessions',
    fuzzy_description = 'find a session… (host:directory): ',
    choices = choices,
    action = wezterm.action_callback(function(w, p, id)
      if not id or id == 'x' then return end
      if id == 'here' then return w:perform_action(act.SpawnTab 'CurrentPaneDomain', p) end
      if id == 'kill' then return kill_picker(w, p) end
      if id:sub(1, 1) == 'n' then return new_session_flow(w, p, id:sub(4), id:sub(2, 2) == 'L') end
      local spec, hostname, sid = id:match('^a|([^|]+)|([^|]+)|(.+)$')
      for _, t in ipairs(w:mux_window():tabs()) do
        for _, pp in ipairs(t:panes()) do
          if pane_session(pp) == sid and pane_host(pp) == hostname then t:activate() return end
        end
      end
      w:perform_action(act.SpawnCommandInNewTab { args = { pult_bin, 'attach', spec } }, p)
    end),
  }, pane)
end

-- Ctrl+Shift+T: new tab on the same host and in the same directory (from a remote tab: on the remote host)
local function new_tab(window, pane, run)
  local host = pane_host(pane)
  local cwd = pane:get_current_working_dir()
  local dir = cwd and cwd.file_path or nil
  if host == my_host or not pane_session(pane) then
    if not run then return window:perform_action(act.SpawnTab 'CurrentPaneDomain', pane) end
    local cmd = { pult_bin, 'new', '--run', run }
    if dir then table.insert(cmd, '--cwd') table.insert(cmd, dir) end
    return window:perform_action(act.SpawnCommandInNewTab { args = cmd }, pane)
  end
  local cmd = { pult_bin, 'new', '--host', host_name_for(fetch_all(), host) }
  if dir then table.insert(cmd, '--cwd') table.insert(cmd, dir) end
  if run then table.insert(cmd, '--run') table.insert(cmd, run) end
  window:perform_action(act.SpawnCommandInNewTab { args = cmd }, pane)
end

-- F4: Claude in this tab if nothing runs in the shell (the daemon's `busy` field), otherwise in a new tab
local function claude_here(window, pane)
  local s
  if pane_session(pane) then
    local _
    _, s = find_pane_session(pane_host(pane) == my_host and fetch_local() or fetch_all(), pane)
  end
  if s and s.busy == false then return pane:send_text('claude\r') end
  new_tab(window, pane, 'claude')
end

-- Ctrl+Shift+W: if something runs in it, choose: detach (it keeps running) or stop
local function close_tab(window, pane)
  local sid = pane_session(pane)
  if not sid then
    window:perform_action(act.CloseCurrentTab { confirm = true }, pane)
    return
  end
  local hosts = pane_host(pane) == my_host and fetch_local() or fetch_all()
  local h, s = find_pane_session(hosts, pane)
  -- an empty shell has nothing worth keeping: close without asking (the session ends)
  if s and s.busy == false then
    window:perform_action(act.CloseCurrentTab { confirm = false }, pane)
    return
  end
  local spec = h and spec_of(h, s) or sid
  window:perform_action(act.InputSelector {
    title = 'Close tab (session ' .. spec .. ')',
    choices = {
      { id = 'detach', label = 'Detach: whatever runs in it keeps running (Ctrl+Shift+O brings it back)' },
      { id = 'kill', label = 'Stop: the session and whatever runs in it exit' },
    },
    action = wezterm.action_callback(function(w, p, id)
      if id == 'detach' then
        w:perform_action(act.CloseCurrentTab { confirm = false }, p)
      elseif id == 'kill' then
        wezterm.run_child_process { pult_bin, 'kill', spec }
      end
    end),
  }, pane)
end

-- Ctrl+V: in a remote session, if the clipboard holds an image, upload it to the host and paste its remote path
-- (Claude attaches a pasted image path as an image). Otherwise Ctrl+V goes through unchanged.
local function paste_image(window, pane)
  local host = pane_host(pane)
  if host ~= my_host and pane_session(pane) then
    local ok, out, err = wezterm.run_child_process { pult_bin, 'paste-image', '--host', host }
    if ok and out ~= '' then return pane:send_paste(out) end
    if not ok and err and err ~= '' then
      window:toast_notification('pult', 'image upload: ' .. err, nil, 4000)
      return
    end
  end
  window:perform_action(act.SendKey { key = 'v', mods = 'CTRL' }, pane)
end

-- F1 / Ctrl+Shift+?: list of keys; type to filter, Enter runs the entry
local function help_picker(window, pane)
  local items = {
    { 'F2  Ctrl+Shift+O', 'sessions on every host: what runs in them, open, new, stop', wezterm.action_callback(session_picker) },
    { 'F3  Ctrl+Shift+I', 'jump to where Claude is waiting for you', wezterm.action_callback(jump_waiting) },
    { 'F4', 'Claude in this tab (if the shell is empty), otherwise in a new tab, same place', wezterm.action_callback(claude_here) },
    { 'F5', 'new session: host → directory → claude or shell', wezterm.action_callback(new_on_host) },
    { 'Ctrl+Shift+T', 'new tab on the same host and in the same directory', wezterm.action_callback(function(w, p) new_tab(w, p) end) },
    { 'Ctrl+Shift+W', 'close the tab (if something runs in it: detach or stop)', wezterm.action_callback(close_tab) },
    { 'Alt+1…9', 'switch tab' },
    { 'F11  Alt+Enter', 'toggle full screen', act.ToggleFullScreen },
    { 'Ctrl+Shift+F', 'search the output', act.Search 'CurrentSelectionOrEmptyString' },
    { 'Ctrl+Shift+Space', 'quick select (URL, hash, path) by letter', act.QuickSelect },
    { 'Ctrl+Shift+V', 'paste', act.PasteFrom 'Clipboard' },
    { 'selection', 'text selected with the mouse is copied right away (Shift+drag if the program grabs the mouse)' },
    { 'Ctrl+V', 'in Claude: paste an image from the clipboard (on a remote host too: it is uploaded there)' },
    { 'Ctrl+Shift+drag', 'move the window' },
    { 'pult ls --all', 'the same from a shell: pult attach devbox:3 · pult new --host devbox --run claude · pult kill 3' },
  }
  local choices = {}
  for i, it in ipairs(items) do
    table.insert(choices, {
      id = tostring(i),
      -- the selected row is drawn with inverted colours, where a coloured key name is unreadable: plain colour +
      -- bold; the padding counts characters (not bytes)
      label = wezterm.format {
        { Attribute = { Intensity = 'Bold' } },
        { Text = it[1] .. string.rep(' ', math.max(1, 20 - utf8.len(it[1]))) }, 'ResetAttributes',
        { Text = it[2] },
      },
    })
  end
  window:perform_action(act.InputSelector {
    title = 'pult keys',
    fuzzy_description = 'key: ',
    choices = choices,
    action = wezterm.action_callback(function(w, p, id)
      local it = id and items[tonumber(id)]
      if it and it[3] then w:perform_action(it[3], p) end
    end),
  }, pane)
end

-- F1–F5 belong to pult and F11 to full screen (as elsewhere); the other F keys stay with the programs (htop, mc, ...)
config.keys = {
  { key = 't', mods = 'CTRL|SHIFT', action = wezterm.action_callback(function(w, p) new_tab(w, p) end) },
  { key = 'w', mods = 'CTRL|SHIFT', action = wezterm.action_callback(close_tab) },
  { key = 'o', mods = 'CTRL|SHIFT', action = wezterm.action_callback(session_picker) },
  { key = 'i', mods = 'CTRL|SHIFT', action = wezterm.action_callback(jump_waiting) },
  { key = 'F1', action = wezterm.action_callback(help_picker) },
  { key = 'F2', action = wezterm.action_callback(session_picker) },
  { key = 'F3', action = wezterm.action_callback(jump_waiting) },
  { key = 'F4', action = wezterm.action_callback(claude_here) },
  { key = 'F5', action = wezterm.action_callback(new_on_host) },
  { key = 'F11', action = act.ToggleFullScreen },
  { key = '?', mods = 'CTRL|SHIFT', action = wezterm.action_callback(help_picker) },
  { key = 'v', mods = 'CTRL', action = wezterm.action_callback(paste_image) },
}
-- Mouse: selecting copies (to the clipboard too, not only the middle-click primary selection); with Shift when the
-- program grabs the mouse. Ctrl+Shift+drag moves the window anywhere.
config.mouse_bindings = {
  { event = { Drag = { streak = 1, button = 'Left' } }, mods = 'CTRL|SHIFT', action = act.StartWindowDrag },
}
for _, mods in ipairs { 'NONE', 'SHIFT' } do
  table.insert(config.mouse_bindings, {
    event = { Up = { streak = 1, button = 'Left' } }, mods = mods,
    action = act.CompleteSelectionOrOpenLinkAtMouseCursor 'ClipboardAndPrimarySelection',
  })
  for _, streak in ipairs { 2, 3 } do
    table.insert(config.mouse_bindings, {
      event = { Up = { streak = streak, button = 'Left' } }, mods = mods,
      action = act.CompleteSelection 'ClipboardAndPrimarySelection',
    })
  end
end

for i = 1, 9 do
  table.insert(config.keys, { key = tostring(i), mods = 'ALT', action = act.ActivateTab(i - 1) })
end

return config
