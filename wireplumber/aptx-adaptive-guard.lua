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

-- Keyed by the device's object.serial (string). PipeWire reuses bound ids, and on
-- device-removed the subject is already unbound (bound-id reads 4294967295); the serial is
-- unique per object and is in the event properties on both add and remove.
-- serial -> true once we've acted; cleared when the device goes away
local attempted = {}
-- serial -> pending settle timer
local pending = {}

-- True when the running codec is a real aptX codec but the current profile, which has its
-- id, is ours. A headset without aptX Adaptive never has our entries in its profiles.
function shouldRecover (codec, profile)
  local id = REAL_CODECS [codec]
  if id == nil or profile == nil then
    return false
  end
  local index = tonumber (profile.index)
  if index == nil or math.floor (index / 65536) ~= A2DP_PROFILE or index % 65536 ~= id then
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

function check (source, serial, dev_id)
  pending [serial] = nil
  if attempted [serial] then
    return
  end
  local device_om = source:call ("get-object-manager", "device")
  local node_om = source:call ("get-object-manager", "node")
  local device = device_om:lookup {
    Constraint { "bound-id", "=", dev_id, type = "gobject" },
    Constraint { "device.api", "=", "bluez5", type = "pw" },
  }
  if device == nil or device.properties ["object.serial"] ~= serial then
    return
  end
  local codec = sinkCodec (node_om, dev_id)
  local profile = currentProfile (device)
  if not shouldRecover (codec, profile) then
    return
  end

  attempted [serial] = true
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
-- dev_id is the device's bound id (string), used to look it and its nodes up.
function schedule (source, serial, dev_id)
  if serial == nil or dev_id == nil or attempted [serial] or pending [serial] then
    return
  end
  pending [serial] = Core.timeout_add (SETTLE_MSEC, function ()
    check (source, serial, dev_id)
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
    local source = event:get_source ()
    local dev_id = event:get_subject ().properties ["device.id"]
    if dev_id == nil then
      return
    end
    local device = source:call ("get-object-manager", "device"):lookup {
      Constraint { "bound-id", "=", dev_id, type = "gobject" },
      Constraint { "device.api", "=", "bluez5", type = "pw" },
    }
    if device ~= nil then
      schedule (source, device.properties ["object.serial"], dev_id)
    end
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
    local device = event:get_subject ()
    schedule (event:get_source (), device.properties ["object.serial"],
        tostring (device ["bound-id"]))
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
    local serial = event:get_properties () ["object.serial"]
    if serial == nil then
      return
    end
    if pending [serial] ~= nil then
      pending [serial]:destroy ()
    end
    pending [serial] = nil
    attempted [serial] = nil
  end
}:register ()
