-- aptx-linux: aptX Adaptive guard (docs/adr/0004-aptx-slot.md, "headset-initiated configuration")
--
-- In the aptx slot our aptX Adaptive entries share the aptX HD and aptX codec ids with the
-- real codecs. PipeWire keys the card's current profile by id, so when a headset that has
-- aptX Adaptive configures the real aptX HD (or aptX) endpoint itself, the card shows the
-- aptX Adaptive profile while aptX HD runs, and picking that profile again is a no-op.
--
-- This hook notices that state and re-selects the profile through "off". The second
-- set_profile differs from the first, so bluez5-device.c switches the codec, and the first
-- supported entry with that id is ours. One reconfigure, at most once per device connection.

cutils = require ("common-utils")
log = Log.open_topic ("s-aptx-adaptive")

-- DEVICE_PROFILE_A2DP in PipeWire 1.6.9 bluez5-device.c; a profile index is profile << 16 | codec id
local A2DP_PROFILE = 2
local OFF_INDEX = 0
local SETTLE_MSEC = 1500
-- real codec name -> its codec id (enum spa_bluetooth_audio_codec), which our entries borrow
local REAL_CODECS = { aptx_hd = 7, aptx = 6 }
-- profile descriptions embed the codec description (build_profile, bluez5-device.c L2104);
-- only our entries' descriptions contain this, the real ones are "aptX", "aptX HD", "aptX-LL"
local OUR_DESCRIPTION = "aptX Adaptive"

-- device bound-id (string) -> true once we've acted; cleared when the device goes away
local attempted = {}
local pending = {}

-- True when the running codec is a real aptX codec but the current profile, which has its
-- id, is ours. A headset without aptX Adaptive never has our entries in its profiles.
function shouldRecover (codec, profile)
  local id = REAL_CODECS [codec]
  if id == nil or profile == nil then
    return false
  end
  local index = tonumber (profile.index)
  if index == nil or index // 65536 ~= A2DP_PROFILE or index % 65536 ~= id then
    return false
  end
  local desc = profile.description
  return desc ~= nil and string.find (desc, OUR_DESCRIPTION, 1, true) ~= nil
end

function currentProfile (device)
  for p in device:iterate_params ("Profile") do
    local profile = cutils.parseParam (p, "Profile")
    if profile ~= nil then
      return profile
    end
  end
  return nil
end

function sinkCodec (node_om, dev_id)
  local node = node_om:lookup {
    Constraint { "device.id", "=", dev_id, type = "pw" },
    Constraint { "media.class", "=", "Audio/Sink", type = "pw-global" },
    Constraint { "api.bluez5.codec", "+", type = "pw" },
  }
  return node and node.properties ["api.bluez5.codec"]
end

function setProfile (device, index)
  device:set_param ("Profile", Pod.Object {
    "Spa:Pod:Object:Param:Profile", "Profile",
    index = index,
    save = false,
  })
end

function check (source, dev_id)
  pending [dev_id] = nil
  if attempted [dev_id] then
    return
  end
  local device_om = source:call ("get-object-manager", "device")
  local node_om = source:call ("get-object-manager", "node")
  local device = device_om:lookup {
    Constraint { "bound-id", "=", dev_id, type = "gobject" },
    Constraint { "device.api", "=", "bluez5", type = "pw" },
  }
  if device == nil then
    return
  end
  local codec = sinkCodec (node_om, dev_id)
  local profile = currentProfile (device)
  if not shouldRecover (codec, profile) then
    return
  end

  attempted [dev_id] = true
  local index = tonumber (profile.index)
  log:info (device, string.format (
      "%s runs %s under profile '%s' (%s); re-selecting it via 'off' to switch to aptX Adaptive",
      tostring (device.properties ["device.name"]), codec, profile.name,
      tostring (profile.description)))
  setProfile (device, OFF_INDEX)
  Core.sync (function ()
    setProfile (device, index)
  end)
end

-- Let WirePlumber's own profile selection settle first; check() looks at the state again.
function schedule (source, dev_id)
  if dev_id == nil or attempted [dev_id] or pending [dev_id] then
    return
  end
  pending [dev_id] = Core.timeout_add (SETTLE_MSEC, function ()
    check (source, dev_id)
    return false
  end)
end

SimpleEventHook {
  name = "aptx-adaptive/guard-real-aptx-node",
  interests = {
    EventInterest {
      Constraint { "event.type", "=", "node-added" },
      Constraint { "media.class", "=", "Audio/Sink" },
      Constraint { "api.bluez5.codec", "=", "aptx_hd" },
    },
    EventInterest {
      Constraint { "event.type", "=", "node-added" },
      Constraint { "media.class", "=", "Audio/Sink" },
      Constraint { "api.bluez5.codec", "=", "aptx" },
    },
  },
  execute = function (event)
    schedule (event:get_source (), event:get_subject ().properties ["device.id"])
  end
}:register ()

-- The profile list can change after the node exists: if the headset configured aptX HD
-- before BlueZ knew its endpoints, the profile only becomes ours once they're known.
SimpleEventHook {
  name = "aptx-adaptive/guard-profiles-changed",
  interests = {
    EventInterest {
      Constraint { "event.type", "=", "device-params-changed" },
      Constraint { "event.subject.param-id", "c", "EnumProfile", "Profile" },
      Constraint { "device.api", "=", "bluez5" },
    },
  },
  execute = function (event)
    schedule (event:get_source (), tostring (event:get_subject () ["bound-id"]))
  end
}:register ()

SimpleEventHook {
  name = "aptx-adaptive/guard-forget-device",
  interests = {
    EventInterest {
      Constraint { "event.type", "=", "device-removed" },
      Constraint { "device.api", "=", "bluez5" },
    },
  },
  execute = function (event)
    local dev_id = tostring (event:get_subject () ["bound-id"])
    if pending [dev_id] ~= nil then
      pending [dev_id]:destroy ()
    end
    pending [dev_id] = nil
    attempted [dev_id] = nil
  end
}:register ()
