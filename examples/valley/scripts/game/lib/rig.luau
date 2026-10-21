--!strict
-- State the scripts share. Modules load once per context, so every script
-- that requires this gets the same table.
--
-- yaw and pitch: where the camera looks (radians; yaw around +Y, pitch above
-- the horizon), so "forward" on the move stick is away from the camera.
-- driving: true while the hero sits in the car.
-- ready: false until the streamed ground under the hero and the car has
-- loaded (player.luau); until then neither moves.
local rig = {
	ready = false,
	yaw = -math.pi * 0.5,
	pitch = 0.3,
	driving = false,
}
return rig
