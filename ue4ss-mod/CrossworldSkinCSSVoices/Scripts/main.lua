-- Crossworld Skin & CSS Manager: separate voices for AI skin slots.
-- Author: SCWS&CSS
-- The game has one voice bank per character for all its AI skins (VOICE_<X>AI). For each AI skin slot that
-- has its own bank (VOICE_<X>A<N>, made by Crossworld Skin & CSS Manager, listed in slots.lua), this script points the
-- character's AI voice lines at that bank whenever the player on this PC picks / drives that slot.
-- The game then plays every line itself (select screen, race, results). Nothing is muted or replayed.

local function log(m) print("[CrossworldSkinCSSVoices] " .. tostring(m) .. "\n") end

-- AI racers in EDriverId order (23..45), with their voice codes; AI Skin N of racer i = 100 + 3*i + (N-1)
local CODES = { "SON", "TAI", "KNU", "AMY", "EGG", "SHA", "ROU", "SIL", "BLA", "MET", "OME", "VEC",
                "ESP", "CHA", "ZAV", "ZAZ", "CRE", "BIG", "EGP", "SAG", "JET", "WAV", "STO" }
local function slotOf(id)
  if type(id) ~= "number" or id < 100 or id > 100 + 3 * #CODES - 1 then return nil end
  local i, n = (id - 100) // 3 + 1, (id - 100) % 3 + 1
  local c = CODES[i]
  return { code = c, n = n, dir = "/AI_CHARA/" .. c .. "AI/", from = "_" .. c:lower() .. "AI_",
           to = "_" .. c:lower() .. "A" .. n .. "_", sheet = "VOICE_" .. c .. "A" .. n }
end

-- slots with their own bank: slots.lua next to this script returns a list of DriverIds
local enabled = {}
do
  local dir = (debug.getinfo(1, "S").source:gsub("^@", ""):match("^(.*)[/\\]") or ".")
  local ok, list = pcall(dofile, dir .. "/slots.lua")
  if not ok then ok, list = pcall(dofile, "ue4ss/Mods/CrossworldSkinCSSVoices/Scripts/slots.lua") end
  if not ok then ok, list = pcall(dofile, "Mods/CrossworldSkinCSSVoices/Scripts/slots.lua") end
  if ok and type(list) == "table" then
    for _, id in ipairs(list) do if slotOf(id) then enabled[id] = slotOf(id) end end
  else
    log("no slots.lua (" .. tostring(list) .. ")")
  end
  local names = {}
  for id, s in pairs(enabled) do names[#names + 1] = s.sheet .. "=" .. id end
  log("slots with their own voice: " .. (#names > 0 and table.concat(names, ", ") or "none"))
end

local KEEP = 0x0E000000
local RF_NEED = 0x400 | 0x1000 | 0x2000   -- NeedLoad | NeedPostLoad | NeedPostLoadSubobjects
local RF_GONE = 0x8000 | 0x10000          -- BeginDestroyed | FinishDestroyed
local function valid(o) return type(o) == "userdata" and o:IsValid() end
-- an object the game is still loading / creating / destroying: don't read anything from it yet
local function settled(o)
  local busy = true
  pcall(function() busy = o:HasAnyFlags(RF_NEED | RF_GONE) end)
  return not busy
end
local function nm(o)
  if type(o) ~= "userdata" then return "nil" end
  local ok, r = pcall(function() if o:IsValid() then return o:GetFullName() end return "nil" end)
  if ok and type(r) == "string" then return r end
  return "?"
end
local function str(v) local ok, r = pcall(function() return v:ToString() end); if ok then return r end return tostring(v) end
local function addr(o) local ok, r = pcall(function() return tostring(o:GetAddress()) end); return ok and r or "?" end

local active = nil      -- slot table in use, or nil
local sheets = {}       -- sheet name -> our loaded cue sheet
local done, seen = {}, {}

local function sheetFor(s)
  if sheets[s.sheet] then return sheets[s.sheet] end
  local ok, e = pcall(function()
    local tmpl = FindFirstOf("SoundAtomCueSheet")
    if not valid(tmpl) then return end
    local cls = StaticFindObject("/Script/CriWareRuntime.SoundAtomCueSheet")
    local ns = StaticConstructObject(cls, FindFirstOf("GameInstance"), 0, 0, KEEP, false, false, tmpl, nil, nil)
    ns.CueSheetName = s.sheet
    pcall(function() ns:MakeSureCueSheetIsLoaded() end)
    local okl, l = pcall(function() return ns:IsLoaded() end)
    log("bank " .. s.sheet .. " loaded=" .. tostring(okl and l))
    sheets[s.sheet] = ns
  end)
  if not ok then log("bank error " .. tostring(e)) end
  return sheets[s.sheet]
end

local lastDone = ""
local function retargetOne(c)
  local s = active
  if not s then return "skip" end
  local key = addr(c)
  if done[key] then return "done" end
  if seen[key] then return "skip" end
  if not settled(c) then return "wait" end
  if not nm(c):find(s.dir, 1, true) then seen[key] = true; return "skip" end
  local okn, name = pcall(function() return str(c.CueName) end)
  if not (okn and name and name ~= "") then return "wait" end
  if not name:find(s.from, 1, true) then seen[key] = true; return "skip" end
  local sheet = sheetFor(s)
  if not sheet then return "wait" end
  local oldSheet = nil
  pcall(function() oldSheet = c.CueSheet end)
  pcall(function() c.CueSheet = sheet; c.CueName = name:gsub(s.from, s.to) end)
  done[key] = { c = c, name = name, sheet = oldSheet }
  lastDone = name .. " | " .. nm(c)
  return "done"
end
local function retargetAll()
  if not active then return end
  local n = 0
  local list = {}
  pcall(function() list = FindAllOf("SoundAtomCue") or {} end)
  for _, c in ipairs(list) do
    -- one bad line must not stop the others from switching
    local ok, r = pcall(function() return valid(c) and not done[addr(c)] and retargetOne(c) end)
    if ok and r == "done" then n = n + 1 end
  end
  if n > 0 then log("voice lines switched: " .. n .. (n <= 3 and ("  (" .. lastDone .. ")") or "")) end
end
local function restoreAll()
  local n = 0
  for key, d in pairs(done) do
    pcall(function() if valid(d.c) then d.c.CueSheet = d.sheet; d.c.CueName = d.name end end)
    done[key] = nil
    n = n + 1
  end
  seen = {}
  if n > 0 then log("voice lines restored: " .. n) end
end
local lastScan = 0
local function use(id, why)
  local s = enabled[id]
  if s == active then return end
  if active then restoreAll() end
  active = s
  if s then
    log("using " .. s.sheet .. " (" .. why .. ", driver " .. tostring(id) .. ")")
    retargetAll()
    lastScan = os.clock()
  else
    log("normal voices (" .. why .. ", driver " .. tostring(id) .. ")")
  end
end

-- the player's own racer: controller -> car -> the driver sitting in it
local function playerDriverId()
  local byCar = {}
  for _, p in ipairs(FindAllOf("BP_DriverPawn_C") or {}) do
    if valid(p) and settled(p) then
      local id = p.DriverId
      pcall(function() local o = p.Owner; if valid(o) then byCar[addr(o)] = id end end)
      pcall(function()
        local rc = p.RootComponent
        local par = valid(rc) and rc.AttachParent or nil
        if valid(par) then local act = par:GetOuter(); if valid(act) then byCar[addr(act)] = byCar[addr(act)] or id end end
      end)
    end
  end
  for _, pc in ipairs(FindAllOf("BP_RacePlayerController_C") or {}) do
    if not (valid(pc) and settled(pc)) then goto next_pc end
    local id = nil
    pcall(function() local pw = pc.Pawn; if valid(pw) then id = byCar[addr(pw)] end end)
    if id then return id end
    ::next_pc::
  end
  return nil
end

local previews = {}
local function tick()
  local inRace = false
  pcall(function() inRace = valid(FindFirstOf("BP_DriverPawn_C")) end)
  if inRace then
    local id = nil
    pcall(function() id = playerDriverId() end)
    if id ~= nil then use(id, "race") end
  else
    -- select screen: the character it just switched to (newest preview, or a preview that changed)
    local newest = nil
    pcall(function()
      for _, r in ipairs(FindAllOf("BP_MenuRacerPreview_C") or {}) do
        if valid(r) then
          local d = r.DriverActor
          if valid(d) and settled(r) and settled(d) then
            local k, id = addr(r), d.DriverId
            if previews[k] ~= id then previews[k] = id; newest = id end
          end
        end
      end
    end)
    if newest ~= nil then use(newest, "select screen") end
  end
  if active and os.clock() - lastScan > 0.5 then retargetAll(); lastScan = os.clock() end
end

-- Lines that load later (race, results, podium) are picked up by the scan in tick(), on the game thread.
-- (No NotifyOnNewObject: it fires on the game's loading thread, and running Lua there at the same time as
-- the game thread corrupts the script's state and crashes the game.)

local function onGameThread()
  local ok, e = pcall(tick)
  if not ok then log("error " .. tostring(e)) end
  return false   -- keep looping
end
if next(enabled) then
  if LoopInGameThreadWithDelay then
    -- runs on the game thread itself: no script code ever runs on another thread
    LoopInGameThreadWithDelay(250, onGameThread)
  else
    LoopAsync(250, function() ExecuteInGameThread(onGameThread); return false end)
  end
end
log("loaded")
