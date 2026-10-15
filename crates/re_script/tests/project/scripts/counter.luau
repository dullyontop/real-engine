local M = ... or {}
M.count = M.count or 0
function M.bump()
	M.count += 1
	return "v1", M.count
end
return M
