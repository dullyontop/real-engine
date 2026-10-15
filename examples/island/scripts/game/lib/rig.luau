--!strict
-- State the camera rig and the player share: where the camera looks (yaw
-- around +Y, pitch above the horizon, in radians), so "forward" on the move
-- stick is away from the camera. Modules load once per context, so both
-- scripts get this same table.
local rig = {
	yaw = 0.0,
	pitch = 0.35,
}
return rig
