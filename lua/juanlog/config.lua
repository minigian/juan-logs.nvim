local M = {
    threshold_size = 1024 * 1024 * 100, -- 100MB trigger
    mode = "dynamic",
    lazy = true, -- background indexing so neovim doesn't freeze
    dynamic_chunk_size = 10000,
    dynamic_margin = 2000, -- reload when we get this close to the edge
    patterns = { "*" },
    enable_custom_statuscol = true,
    syntax = false
}

function M.setup(user_config)
    if user_config then
        for k, v in pairs(user_config) do
            M[k] = v
        end
    end
end

return M