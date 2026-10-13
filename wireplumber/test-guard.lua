-- Offline test for aptx-adaptive-guard.lua: stubs the WirePlumber globals, loads the script,
-- and drives its hooks with fake devices and nodes. Run: lua wireplumber/test-guard.lua

local dir = arg [0]:match ("(.*/)") or "./"

local hooks = {}
local logs = {}
local timers = {}
local set_params = {}

package.preload ["common-utils"] = function ()
  return {
    parseParam = function (p, id)
      if p.id == id then return p.props end
      return nil
    end,
  }
end

Log = { open_topic = function ()
  return { info = function (_, _, msg) table.insert (logs, msg) end }
end }
Constraint = function (t) return t end
EventInterest = function (t) return t end
SimpleEventHook = function (spec)
  return { register = function () hooks [spec.name] = spec end }
end
Pod = { Object = function (t) return t end }
Core = {
  sync = function (f) f () end,
  timeout_add = function (_, f)
    local t = { f = f, destroyed = false }
    t.destroy = function (self) self.destroyed = true end
    table.insert (timers, t)
    return t
  end,
}

dofile (dir .. "aptx-adaptive-guard.lua")

local function matches (obj_props, constraints)
  for _, c in ipairs (constraints) do
    local key, op, value = c [1], c [2], c [3]
    local v = obj_props [key]
    if op == "=" and tostring (v) ~= tostring (value) then return false end
    if op == "+" and v == nil then return false end
  end
  return true
end

local function om (objects)
  return { lookup = function (_, constraints)
    for _, o in ipairs (objects) do
      if matches (o.match, constraints) then return o end
    end
    return nil
  end }
end

-- Live on WirePlumber 0.5.18 (probe hook on a private instance): a device keeps its bound
-- id across reconnects but gets a new object.serial, and on device-removed the subject is
-- already unbound (bound-id 4294967295, no properties); object.serial and object.id are
-- only in the event properties.
local UNBOUND = 4294967295
local next_serial = 100
local current_serial

local function scenario (codec, profile_index, profile_desc)
  next_serial = next_serial + 1
  current_serial = tostring (next_serial)
  local device = {
    match = { ["bound-id"] = 42, ["device.api"] = "bluez5" },
    properties = { ["device.name"] = "bluez_card.00_11", ["object.serial"] = current_serial },
    ["bound-id"] = 42,
  }
  device.iterate_params = function (_, id)
    local done = false
    return function ()
      if done or id ~= "Profile" then return nil end
      done = true
      return { id = "Profile", props = {
        index = profile_index, name = "a2dp-sink", description = profile_desc } }
    end
  end
  device.set_param = function (_, id, pod)
    table.insert (set_params, { id = id, index = pod.index, save = pod.save })
  end
  local node = {
    match = { ["device.id"] = "42", ["media.class"] = "Audio/Sink", ["api.bluez5.codec"] = codec },
    properties = { ["device.id"] = "42", ["api.bluez5.codec"] = codec },
  }
  local source = { call = function (_, what, kind)
    assert (what == "get-object-manager")
    if kind == "device" then return om { device } end
    return om { node }
  end }
  return device, node, source
end

local function node_added (node, source)
  hooks ["aptx-adaptive/guard-real-aptx-node"].execute {
    get_subject = function () return node end,
    get_source = function () return source end,
  }
end

local function params_changed (device, source)
  hooks ["aptx-adaptive/guard-profiles-changed"].execute {
    get_subject = function () return device end,
    get_source = function () return source end,
  }
end

local function run_timers ()
  local ts = timers
  timers = {}
  for _, t in ipairs (ts) do
    if not t.destroyed then t.f () end
  end
end

-- The device disconnects: device-removed as WirePlumber delivers it.
local function device_removed ()
  hooks ["aptx-adaptive/guard-forget-device"].execute {
    get_subject = function () return { ["bound-id"] = UNBOUND, properties = {} } end,
    get_properties = function ()
      return { ["object.id"] = "42", ["object.serial"] = current_serial, ["device.api"] = "bluez5" }
    end,
  }
end

local function reset ()
  device_removed ()
  set_params, logs, timers = {}, {}, {}
end

local OURS = "High Fidelity Playback (A2DP Sink, codec aptX Adaptive)"
local OURS_LL = "High Fidelity Playback (A2DP Sink, codec aptX Adaptive (Low Latency))"
local REAL_HD = "High Fidelity Playback (A2DP Sink, codec aptX HD)"
local A2DP_HD = 2 * 65536 + 7
local A2DP_APTX = 2 * 65536 + 6

-- real aptX HD running under our profile: off, then the same profile again, once
local _, node, source = scenario ("aptx_hd", A2DP_HD, OURS)
node_added (node, source)
assert (#set_params == 0, "waits for the settle timer")
run_timers ()
assert (#set_params == 2, "acts")
assert (set_params [1].index == 0 and set_params [2].index == A2DP_HD)
assert (set_params [1].save == false and set_params [2].save == false)
assert (#logs == 1 and logs [1]:find ("aptx_hd", 1, true))
node_added (node, source)
run_timers ()
assert (#set_params == 2, "once per connection")

-- a new connection (same bound id, new serial) may act again
reset ()
_, node, source = scenario ("aptx_hd", A2DP_HD, OURS)
node_added (node, source)
run_timers ()
assert (#set_params == 2, "acts after the device is re-added")

-- real aptX under our LL alias profile
reset ()
_, node, source = scenario ("aptx", A2DP_APTX, OURS_LL)
node_added (node, source)
run_timers ()
assert (#set_params == 2 and set_params [2].index == A2DP_APTX)

-- a headset without aptX Adaptive: its aptX HD profile is the real one, never touched
reset ()
_, node, source = scenario ("aptx_hd", A2DP_HD, REAL_HD)
node_added (node, source)
run_timers ()
assert (#set_params == 0 and #logs == 0, "non-Adaptive headset untouched")

-- our codec running: nothing to do
reset ()
_, node, source = scenario ("aptx_adaptive", A2DP_HD, OURS)
node_added (node, source)
run_timers ()
assert (#set_params == 0)

-- not an A2DP profile (e.g. headset-head-unit, profile 3)
reset ()
_, node, source = scenario ("aptx_hd", 3 * 65536 + 7, OURS)
node_added (node, source)
run_timers ()
assert (#set_params == 0)

-- the device goes away before the timer fires
reset ()
_, node, source = scenario ("aptx_hd", A2DP_HD, OURS)
node_added (node, source)
local timer = timers [1]
device_removed ()
assert (timer ~= nil and timer.destroyed, "pending timer destroyed on removal")
run_timers ()
assert (#set_params == 0, "pending check cancelled")

-- running codec and profile id disagree: not our case
reset ()
_, node, source = scenario ("aptx", A2DP_HD, OURS)
node_added (node, source)
run_timers ()
assert (#set_params == 0, "codec id must match the profile")

-- the profile becomes ours only after the node exists (EnumProfile/Profile changed)
reset ()
local device
device, node, source = scenario ("aptx_hd", A2DP_HD, OURS)
params_changed (device, source)
run_timers ()
assert (#set_params == 2, "acts on a profile change too")
params_changed (device, source)
node_added (node, source)
run_timers ()
assert (#set_params == 2, "our own off/on switch doesn't retrigger")

print ("aptx-adaptive-guard: ok")
