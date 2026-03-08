local core = require("juanlog.core")
local ffi_mod = require("juanlog.ffi")
local config = require("juanlog.config")
local commands = require("juanlog.commands")

local M = {}

local function attach_buffer_events(bufnr, state, filepath)
    local lib = ffi_mod.get_lib()

    -- listen for edits and send them to the rust piece table
    vim.api.nvim_buf_attach(bufnr, false, {
        on_lines = function(_, _, _, firstline, lastline, new_lastline)
            if state.updating or state.indexing_progress < 1.0 then return end
            
            local start_line = state.offset + firstline
            local num_deleted = lastline - firstline
            
            local new_lines = vim.api.nvim_buf_get_lines(bufnr, firstline, new_lastline, false)
            local new_text = table.concat(new_lines, "\n")

            lib.log_engine_apply_edit(state.engine, start_line, num_deleted, new_text)
            state.total = tonumber(lib.log_engine_total_lines(state.engine))
        end
    })

    -- hijack save command. now with 100% less UI freezing.
    vim.api.nvim_create_autocmd("BufWriteCmd", {
        buffer = bufnr,
        callback = function()
            -- anti-spam shield. one save at a time.
            if state.save_progress >= 0.0 then return end
            
            -- remember the exact moment we started saving to prevent data loss.
            state.save_tick = vim.b[bufnr].changedtick
            vim.notify("[JuanLog] Saving in background...", vim.log.levels.INFO)
            vim.b[bufnr].juanlog_status = "Saving..."
            
            if lib.log_engine_save_async(state.engine, filepath) then
                state.save_progress = 0.0
                state.save_timer = vim.loop.new_timer()
                state.save_timer:start(100, 100, vim.schedule_wrap(function()
                    if not vim.api.nvim_buf_is_valid(bufnr) then
                        state.save_timer:stop()
                        state.save_timer:close()
                        return
                    end
                    
                    local p = lib.log_engine_get_save_progress(state.engine)
                    if p < 0.0 then
                        state.save_timer:stop()
                        state.save_timer:close()
                        state.save_progress = -1.0
                        vim.b[bufnr].juanlog_status = nil
                        
                        -- only clear modified flag if the user didn't type anything while we were saving.
                        if vim.b[bufnr].changedtick == state.save_tick then
                            vim.api.nvim_buf_set_option(bufnr, 'modified', false)
                            vim.notify("[JuanLog] Save complete.", vim.log.levels.INFO)
                        else
                            vim.notify("[JuanLog] Saved (but there are unsaved changes).", vim.log.levels.WARN)
                        end
                    else
                        state.save_progress = p
                        vim.b[bufnr].juanlog_status = string.format("Saving: %d%%", math.floor(p * 100))
                    end
                    vim.cmd("redrawstatus")
                end))
            end
        end
    })

    -- infinite scrolling magic. 
    -- if cursor hits the margin, fetch next/prev chunk and shift everything.
    vim.api.nvim_create_autocmd({"CursorMoved", "CursorMovedI"}, {
        buffer = bufnr,
        callback = function()
            if state.updating then return end
            
            -- if we are in the abyss, don't try to shift. we don't know where we are.
            if state.is_eof_mode then return end
            
            state.timer:stop()
            state.timer:start(15, 0, vim.schedule_wrap(function()
                if state.updating or not vim.api.nvim_buf_is_valid(bufnr) then return end

                local cursor = vim.api.nvim_win_get_cursor(0)
                local row = cursor[1]
                local buf_lines = vim.api.nvim_buf_line_count(bufnr)
                
                local shift_needed = false
                local new_offset = state.offset

                -- hit bottom margin?
                if row > (buf_lines - config.dynamic_margin) and (state.offset + buf_lines < state.total) then
                    local shift_amount = math.floor(config.dynamic_chunk_size / 2)
                    new_offset = state.offset + shift_amount
                    
                    if new_offset + config.dynamic_chunk_size > state.total then
                        new_offset = state.total - config.dynamic_chunk_size
                    end
                    shift_needed = true
                end

                -- hit top margin?
                if row < config.dynamic_margin and state.offset > 0 then
                    local shift_amount = math.floor(config.dynamic_chunk_size / 2)
                    new_offset = math.max(0, state.offset - shift_amount)
                    shift_needed = true
                end

                if shift_needed and new_offset ~= state.offset then
                    state.updating = true
                    local was_modified = vim.api.nvim_buf_get_option(bufnr, 'modified')
                    
                    local new_lines = core.fetch_lines(state.engine, new_offset, config.dynamic_chunk_size)
                    
                    -- swap buffer content seamlessly
                    core.force_set_lines(bufnr, 0, -1, false, new_lines)
                    
                    -- adjust cursor relative to the new window
                    local new_row = (state.offset + row) - new_offset
                    new_row = math.max(1, math.min(new_row, #new_lines))
                    
                    core.safe_set_cursor(0, {new_row, cursor[2]})
                    
                    state.offset = new_offset
                    vim.api.nvim_buf_set_option(bufnr, 'modified', was_modified)
                    state.updating = false
                end
            end))
        end
    })
end

local function finish_indexing(bufnr, state, filepath, is_complete)
    local lib = ffi_mod.get_lib()
    state.total = tonumber(lib.log_engine_total_lines(state.engine))
    
    state.updating = true
    local initial_lines = core.fetch_lines(state.engine, 0, config.dynamic_chunk_size)
    core.force_set_lines(bufnr, 0, -1, false, initial_lines)
    vim.api.nvim_buf_set_option(bufnr, 'modified', false)
    state.updating = false

    core.safe_set_cursor(0, {1, 0})
    vim.cmd("redraw!")

    vim.api.nvim_buf_set_option(bufnr, 'modifiable', true)
    
    if is_complete then
        vim.notify("[JuanLog] Indexing complete. Total lines: " .. state.total, vim.log.levels.INFO)
    else
        vim.notify("[JuanLog] File opened on-demand. Indexing in background...", vim.log.levels.INFO)
    end

    attach_buffer_events(bufnr, state, filepath)
end

local function setup_dynamic_window(bufnr, engine, total_lines, filepath)
    local lib = ffi_mod.get_lib()
    local state = {
        offset = 0,
        total = total_lines,
        bufnr = bufnr,
        engine = engine,
        updating = false, -- semaphore to prevent recursion loops
        last_query = nil,
        timer = vim.loop.new_timer(),
        indexing_progress = 0.0,
        is_eof_mode = false,
        poll_timer = nil,
        save_progress = -1.0,
        save_timer = nil,
        save_tick = 0
    }
    _G.JuanLogStates[bufnr] = state

    local winid = vim.fn.bufwinid(bufnr)
    if winid ~= -1 then
        if config.enable_custom_statuscol then
            vim.wo[winid].statuscolumn = "%!v:lua._juan_log_statuscol()"
            vim.wo[winid].number = true
        end
        vim.wo[winid].statusline = "%f %h%m%r %= %{get(b:,'juanlog_status','')} %15(%l/%L%)"
    end

    local progress = lib.log_engine_get_progress(engine)
    state.indexing_progress = progress

    if not config.lazy and progress < 1.0 then
        vim.api.nvim_buf_set_option(bufnr, 'modifiable', false)
        core.force_set_lines(bufnr, 0, -1, false, { string.format("[INDEXING... %d%%]", math.floor(progress * 100)) })
        
        state.poll_timer = vim.loop.new_timer()
        state.poll_timer:start(100, 100, vim.schedule_wrap(function()
            if not vim.api.nvim_buf_is_valid(bufnr) then
                state.poll_timer:stop()
                state.poll_timer:close()
                return
            end
            
            local p = lib.log_engine_get_progress(state.engine)
            state.indexing_progress = p
            
            if p >= 1.0 then
                state.poll_timer:stop()
                state.poll_timer:close()
                finish_indexing(bufnr, state, filepath, true)
            else
                core.force_set_lines(bufnr, 0, -1, false, { string.format("[INDEXING... %d%%]", math.floor(p * 100)) })
            end
        end))
    else
        finish_indexing(bufnr, state, filepath, progress >= 1.0)
        
        if progress < 1.0 then
            state.poll_timer = vim.loop.new_timer()
            state.poll_timer:start(100, 100, vim.schedule_wrap(function()
                if not vim.api.nvim_buf_is_valid(bufnr) then
                    state.poll_timer:stop()
                    state.poll_timer:close()
                    return
                end
                
                local p = lib.log_engine_get_progress(state.engine)
                state.indexing_progress = p
                state.total = tonumber(lib.log_engine_total_lines(state.engine))
                
                if p >= 1.0 then
                    state.poll_timer:stop()
                    state.poll_timer:close()
                    vim.notify("[JuanLog] Indexing complete. Total lines: " .. state.total, vim.log.levels.INFO)
                    if state.is_eof_mode then
                        core.jump_to_eof(bufnr, state)
                    end
                end
                vim.cmd("redrawstatus")
            end))
        end
    end
end

function M.attach_to_buffer(bufnr, filepath)
    local lib = ffi_mod.get_lib()
    if not lib then return end

    -- Telescope tries to open a 50GB file in a floating window. We say no.
    local bt = vim.api.nvim_buf_get_option(bufnr, 'buftype')
    if bt ~= "" and bt ~= "acwrite" then return end

    for _, win in ipairs(vim.fn.win_findbuf(bufnr)) do
        local win_config = vim.api.nvim_win_get_config(win)
        if win_config.relative ~= "" then
            return
        end
    end

    local engine = lib.log_engine_new(filepath, config.lazy)
    if engine == nil then return end

    local total_lines = tonumber(lib.log_engine_total_lines(engine))

    vim.api.nvim_buf_set_option(bufnr, 'buftype', 'acwrite')
    vim.api.nvim_buf_set_option(bufnr, 'swapfile', false)
    vim.api.nvim_buf_set_name(bufnr, filepath)
    
    -- turn off expensive stuff for huge files
    if not config.syntax then
        pcall(function() vim.opt_local.syntax = "off" end)
    else
        local ft = vim.filetype.match({ filename = filepath })
        if ft then
            vim.api.nvim_buf_set_option(bufnr, 'filetype', ft)
        end
    end
    pcall(function() vim.opt_local.spell = false end)

    if config.mode == "load_all" then
        core.load_all_lines(bufnr, engine, total_lines)
    else
        setup_dynamic_window(bufnr, engine, total_lines, filepath)
        commands.setup_commands(bufnr)
    end

    vim.api.nvim_create_autocmd("BufWipeout", {
        buffer = bufnr,
        callback = function()
            local state = _G.JuanLogStates[bufnr]
            if state then
                if state.timer then
                    state.timer:stop()
                    state.timer:close()
                end
                if state.poll_timer then
                    state.poll_timer:stop()
                    state.poll_timer:close()
                end
                if state.save_timer then
                    state.save_timer:stop()
                    state.save_timer:close()
                end
            end
            -- trigger the cyanide pill in rust
            lib.log_engine_free(engine)
            _G.JuanLogStates[bufnr] = nil
        end
    })
end

return M