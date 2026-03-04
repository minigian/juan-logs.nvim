local core = require("juanlog.core")
local ffi_mod = require("juanlog.ffi")

local M = {}

function M.setup_commands(bufnr)
    local lib = ffi_mod.get_lib()

    local function do_search(query)
        local state = _G.JuanLogStates[bufnr]
        if not state or not query or query == "" then return end
        
        if state.indexing_progress < 1.0 then
            vim.notify("[JuanLog] Searching only in the indexed " .. math.floor(state.indexing_progress * 100) .. "%...", vim.log.levels.WARN)
        end
        
        state.last_query = query

        local cursor = vim.api.nvim_win_get_cursor(0)
        local current_line_idx = state.offset + cursor[1] - 1 
        
        -- try to find the closest match (up or down)
        local start_down = current_line_idx + 1
        local found_down = tonumber(lib.log_engine_search(state.engine, query, start_down))

        local start_up = math.max(0, current_line_idx - 1)
        local found_up = -1
        
        if current_line_idx > 0 then
            found_up = tonumber(lib.log_engine_search_backward(state.engine, query, start_up))
        end

        local target_line = -1

        if found_down >= 0 and found_up >= 0 then
            local dist_down = found_down - current_line_idx
            local dist_up = current_line_idx - found_up
            if dist_up < dist_down then
                target_line = found_up
            else
                target_line = found_down
            end
        elseif found_down >= 0 then
            target_line = found_down
        elseif found_up >= 0 then
            target_line = found_up
        end

        if target_line >= 0 then
            core.jump_to_line(bufnr, state, target_line)
        end
    end

    -- standard / search won't work because lines aren't loaded.
    -- implementing custom search commands that query the engine.
    vim.api.nvim_buf_create_user_command(bufnr, "Logfind", function(opts)
        do_search(opts.args)
    end, { nargs = 1 })
    
    -- More similar to Neovim, you're welcome
    vim.keymap.set("n", "/", function()
        vim.ui.input({ prompt = "/" }, function(input)
            if input then
                do_search(input)
            end
        end)
    end, { buffer = bufnr, silent = true })

    -- how many lines did we actually parse?
    vim.api.nvim_buf_create_user_command(bufnr, "LogLines", function()
        local state = _G.JuanLogStates[bufnr]
        if state then
            if state.indexing_progress < 1.0 then
                vim.notify("[JuanLog] Indexing... (~" .. state.total .. " lines so far)", vim.log.levels.INFO)
            else
                vim.notify("[JuanLog] Total lines: " .. state.total, vim.log.levels.INFO)
            end
        end
    end, {})

    -- teleport to absolute line. vim's native :1234 won't work here.
    vim.api.nvim_buf_create_user_command(bufnr, "LogJump", function(opts)
        local state = _G.JuanLogStates[bufnr]
        if not state then return end
        local target = tonumber(opts.args)
        if target and target > 0 and target <= state.total then
            core.jump_to_line(bufnr, state, target - 1)
        end
    end, { nargs = 1 })

    -- remap 'n' and 'N'
    vim.keymap.set("n", "n", function()
        local state = _G.JuanLogStates[bufnr]
        if not state or not state.last_query then return end

        local cursor = vim.api.nvim_win_get_cursor(0)
        local start_line = state.offset + cursor[1]

        local found_line = tonumber(lib.log_engine_search(state.engine, state.last_query, start_line))

        if found_line >= 0 then
            core.jump_to_line(bufnr, state, found_line)
        end
    end, { buffer = bufnr, silent = true })

    vim.keymap.set("n", "N", function()
        local state = _G.JuanLogStates[bufnr]
        if not state or not state.last_query then return end

        local cursor = vim.api.nvim_win_get_cursor(0)
        local current_abs_line = state.offset + cursor[1] - 1
        
        if current_abs_line <= 0 then 
            return 
        end

        local start_line = current_abs_line - 1
        local found_line = tonumber(lib.log_engine_search_backward(state.engine, state.last_query, start_line))

        if found_line >= 0 then
            core.jump_to_line(bufnr, state, found_line)
        end
    end, { buffer = bufnr, silent = true })

    -- hijack gg to go to the actual start of the file
    vim.keymap.set("n", "gg", function()
        local state = _G.JuanLogStates[bufnr]
        if state then core.jump_to_line(bufnr, state, 0) end
    end, { buffer = bufnr, silent = true })

    -- hijack G to go to the actual end of the file
    vim.keymap.set("n", "G", function()
        local state = _G.JuanLogStates[bufnr]
        if state then core.jump_to_eof(bufnr, state) end
    end, { buffer = bufnr, silent = true })
end

return M