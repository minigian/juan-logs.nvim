local core = require("juanlog.core")
local ffi_mod = require("juanlog.ffi")
local ffi = require("ffi")
local config = require("juanlog.config")

local M = {}

function M.setup_commands(bufnr)
    local lib = ffi_mod.get_lib()

    local function do_search(query)
        local state = _G.JuanLogStates[bufnr]
        if not state or not query or query == "" then return end
        
        if state.indexing_progress < 1.0 then
            vim.notify("[JuanLog] File is still loading. Searching only in currently indexed zones.", vim.log.levels.WARN)
        end
        
        local base_query = query
        local limit_start = 0
        local limit_end = state.total

        local q, s, e = query:match("^(.-)%s+%-r%s+(%d+)%s+(%d+)$")
        if not q then
            q, s, e = query:match("^(.-)%s+%-%-range%s+(%d+)%s+(%d+)$")
        end

        if q then
            base_query = q
            limit_start = math.max(0, tonumber(s) - 1)
            limit_end = math.min(state.total, tonumber(e) - 1)
        else
            local q_p, s_p, e_p = query:match("^(.-)%s+%-rp%s+(%d+)%s+(%d+)$")
            if not q_p then
                q_p, s_p, e_p = query:match("^(.-)%s+%-%-rangeper%s+(%d+)%s+(%d+)$")
            end
            
            if q_p then
                base_query = q_p
                local p_start = math.max(0, math.min(100, tonumber(s_p)))
                local p_end = math.max(0, math.min(100, tonumber(e_p)))
                
                if p_start > p_end then
                    p_start, p_end = p_end, p_start
                end
                
                limit_start = math.floor((p_start / 100) * state.total)
                limit_end = math.floor((p_end / 100) * state.total)
                
                limit_start = math.max(0, limit_start)
                limit_end = math.min(state.total, limit_end)
            else
                local q_b, up_b, down_b = query:match("^(.-)%s+%-b%s+(%d+)%s+(%d+)$")
                if not q_b then
                    local q_b_single, val_b = query:match("^(.-)%s+%-b%s+(%d+)$")
                    if q_b_single then
                        q_b = q_b_single
                        up_b = val_b
                        down_b = val_b
                    else
                        local q_b_flag = query:match("^(.-)%s+%-b$")
                        if q_b_flag then
                            q_b = q_b_flag
                            up_b = 1
                            down_b = 1
                        end
                    end
                end

                if q_b then
                    base_query = q_b
                    local chunk_size = state.chunk_size or config.dynamic_chunk_size
                    limit_start = math.max(0, state.offset - (tonumber(up_b) * chunk_size))
                    limit_end = math.min(state.total, state.offset + chunk_size + (tonumber(down_b) * chunk_size) - 1)
                end
            end
        end

        state.last_query = base_query
        state.last_limit_start = limit_start
        state.last_limit_end = limit_end

        local cursor = vim.api.nvim_win_get_cursor(0)
        local current_line_idx = state.offset + cursor[1] - 1 
        
        -- try to find the closest match (up or down)
        local start_down = current_line_idx + 1
        if start_down < limit_start then start_down = limit_start end
        
        local found_down = -1
        if start_down <= limit_end then
            found_down = tonumber(lib.log_engine_search(state.engine, base_query, start_down, limit_end))
        end

        local start_up = math.max(0, current_line_idx - 1)
        if start_up > limit_end then start_up = limit_end end
        
        local found_up = -1
        if start_up >= limit_start and current_line_idx > 0 then
            found_up = tonumber(lib.log_engine_search_backward(state.engine, base_query, start_up, limit_start))
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
        else
            vim.notify("[JuanLog] Pattern not found: " .. base_query, vim.log.levels.INFO)
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

    vim.api.nvim_buf_create_user_command(bufnr, "Logstats", function()
        local state = _G.JuanLogStates[bufnr]
        if not state then return end
        
        local stats = ffi.new("LogStats")
        if lib.log_engine_get_stats(state.engine, stats) then
            local progress = tonumber(stats.progress) * 100
            local lines = tonumber(stats.total_lines)
            local bytes = tonumber(stats.file_size_bytes)
            local time_ms = tonumber(stats.indexing_time_ms)
            
            local size_str = string.format("%.2f MB", bytes / (1024 * 1024))
            if bytes > 1024 * 1024 * 1024 then
                size_str = string.format("%.2f GB", bytes / (1024 * 1024 * 1024))
            end
            
            local time_str = string.format("%.2f seconds", time_ms / 1000)
            if progress < 100 then
                time_str = "Indexing in progress..."
            end
            
            local msg = string.format(
                "[JuanLog Stats]\nProgress: %d%%\nTotal Lines: %d\nFile Size: %s\nIndexing Time: %s",
                math.floor(progress), lines, size_str, time_str
            )
            vim.notify(msg, vim.log.levels.INFO)
        end
    end, {})

    -- teleport to absolute line. vim's native :1234 won't work here.
    vim.api.nvim_buf_create_user_command(bufnr, "LogJump", function(opts)
        local state = _G.JuanLogStates[bufnr]
        if not state then return end
        local target = tonumber(opts.args)
        if target and target > 0 then
            if state.indexing_progress < 1.0 and target > state.total then
                vim.notify("[JuanLog] Line " .. target .. " is not indexed yet. Max available: " .. state.total, vim.log.levels.WARN)
                return
            end
            if target <= state.total then
                core.jump_to_line(bufnr, state, target - 1)
            end
        end
    end, { nargs = 1 })

    -- remap 'n' and 'N'
    vim.keymap.set("n", "n", function()
        local state = _G.JuanLogStates[bufnr]
        if not state or not state.last_query then return end

        local cursor = vim.api.nvim_win_get_cursor(0)
        local start_line = state.offset + cursor[1]
        
        local limit_end = state.last_limit_end or state.total

        if start_line <= limit_end then
            local found_line = tonumber(lib.log_engine_search(state.engine, state.last_query, start_line, limit_end))
            if found_line >= 0 then
                core.jump_to_line(bufnr, state, found_line)
            else
                vim.notify("[JuanLog] No more matches downwards.", vim.log.levels.INFO)
            end
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
        local limit_start = state.last_limit_start or 0

        if start_line >= limit_start then
            local found_line = tonumber(lib.log_engine_search_backward(state.engine, state.last_query, start_line, limit_start))
            if found_line >= 0 then
                core.jump_to_line(bufnr, state, found_line)
            else
                vim.notify("[JuanLog] No more matches upwards.", vim.log.levels.INFO)
            end
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